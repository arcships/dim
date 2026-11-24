use crate::host::{HostError, HostErrorKind, SessionFileError, SessionFileErrorCode, SessionFiles};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_types::{
    ContentBlock, ImageSource, ImageSourceType, MessageContent, MessagePart, ModelRequest,
    ToolContent,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The Session file limit on one image, in decoded bytes.
const IMAGE_LIMIT: u64 = 16 * 1024 * 1024;

/// Materializes a durable image at an external protocol boundary.
pub async fn image(
    files: &SessionFiles,
    session: &str,
    source: &ImageSource,
    cancel: &CancellationToken,
) -> Result<ImageSource, HostError> {
    let id = source.file_id.as_ref().ok_or_else(|| {
        HostError::classified(HostErrorKind::InvalidRequest, "Image file id is missing")
    })?;
    let mut bytes = Vec::new();
    loop {
        let chunk = files
            .0
            .read(
                session.to_owned(),
                id.clone(),
                bytes.len() as u64,
                cancel.clone(),
            )
            .await
            .map_err(|e| match e.code {
                SessionFileErrorCode::Cancelled => HostError::Cancelled(e.message),
                SessionFileErrorCode::NodeUnavailable => {
                    HostError::classified(HostErrorKind::Transport, e.message)
                }
                _ => HostError::failed(e.message),
            })?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend(chunk);
        if bytes.len() as u64 > IMAGE_LIMIT {
            return Err(HostError::classified(
                HostErrorKind::InvalidRequest,
                "Image file exceeds its storage limit",
            ));
        }
    }

    Ok(ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: source.media_type.clone(),
        data: Some(STANDARD.encode(bytes)),
        url: None,
        file_id: None,
    })
}

/// Resolves only the images selected for this model request; the stored request stays immutable.
pub async fn materialize(
    request: &Arc<ModelRequest>,
    files: Option<&SessionFiles>,
    cancel: &CancellationToken,
) -> Result<Arc<ModelRequest>, HostError> {
    let mut changed = None;
    let mut image_bytes = request
        .messages
        .iter()
        .flat_map(|message| match &message.content {
            MessageContent::Parts(parts) => parts.as_slice(),
            _ => &[],
        })
        .map(|part| match part {
            MessagePart::Image { source } => source.data.as_ref().map_or(0, String::len),
            MessagePart::ToolResult {
                content: ToolContent::Blocks(blocks),
                ..
            } => blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Image { source } => source.data.as_ref().map_or(0, String::len),
                    _ => 0,
                })
                .sum(),
            _ => 0,
        })
        .sum::<usize>();
    check_image_budget(image_bytes)?;
    for (index, message) in request.messages.iter().enumerate() {
        let MessageContent::Parts(parts) = &message.content else {
            continue;
        };
        if !parts.iter().any(|part| match part {
            MessagePart::Image { source } => source.source_type == ImageSourceType::File,
            MessagePart::ToolResult { content: ToolContent::Blocks(blocks), .. } => blocks.iter().any(|block| matches!(block, ContentBlock::Image {source} if source.source_type == ImageSourceType::File)),
            _ => false,
        }) {
            continue;
        }
        let mut copy = parts.clone();
        for part in &mut copy {
            match part {
                MessagePart::Image { source } if source.source_type == ImageSourceType::File => {
                    *source = resolve(request, files, source, cancel).await?;
                    image_bytes += source.data.as_ref().map_or(0, String::len);
                    check_image_budget(image_bytes)?;
                }
                MessagePart::ToolResult {
                    content: ToolContent::Blocks(blocks),
                    ..
                } => {
                    for block in blocks {
                        if let ContentBlock::Image { source } = block
                            && source.source_type == ImageSourceType::File
                        {
                            *source = resolve(request, files, source, cancel).await?;
                            image_bytes += source.data.as_ref().map_or(0, String::len);
                            check_image_budget(image_bytes)?;
                        }
                    }
                }
                _ => {}
            }
        }
        let messages = changed.get_or_insert_with(|| request.messages.clone());
        let mut message = (**message).clone();
        message.content = MessageContent::Parts(copy);
        messages[index] = Arc::new(message);
    }
    match changed {
        None => Ok(request.clone()),
        Some(messages) => {
            let mut copy = (**request).clone();
            copy.messages = messages;
            Ok(Arc::new(copy))
        }
    }
}

/// The most encoded image data one model request carries, user and
/// tool-result images together.
#[cfg(not(test))]
const IMAGE_BUDGET: usize = 32 * 1024 * 1024;
/// The unit-test target crosses the same rule at a cap it reaches without
/// allocating production-sized images; integration tests see the real limit.
#[cfg(test)]
const IMAGE_BUDGET: usize = 128;

pub(crate) fn check_image_budget(bytes: usize) -> Result<(), HostError> {
    if bytes > IMAGE_BUDGET {
        return Err(HostError::classified(
            HostErrorKind::InvalidRequest,
            "Model request images exceed 32 MiB of encoded data",
        ));
    }
    Ok(())
}

pub(crate) async fn resolve(
    request: &ModelRequest,
    files: Option<&SessionFiles>,
    source: &ImageSource,
    cancel: &CancellationToken,
) -> Result<ImageSource, HostError> {
    let files = files.ok_or_else(|| {
        HostError::classified(
            HostErrorKind::InvalidRequest,
            "Session file service is unavailable",
        )
    })?;
    let session = request
        .metadata
        .as_ref()
        .and_then(|m| m.session_id.as_deref())
        .ok_or_else(|| {
            HostError::classified(
                HostErrorKind::InvalidRequest,
                "Image file requires a Session identity",
            )
        })?;
    image(files, session, source, cancel).await
}

/// Stores image bytes received at a tool boundary before publishing its result.
pub async fn store_result(
    files: &SessionFiles,
    session: &str,
    result: &mut dim_agent_types::ToolResult,
    cancel: &CancellationToken,
) -> Result<(), HostError> {
    let ToolContent::Blocks(blocks) = &mut result.content else {
        return Ok(());
    };
    for block in blocks {
        if let ContentBlock::Image { source } = block {
            if source.source_type != ImageSourceType::Base64 {
                continue;
            }
            let data = source.data.as_deref().ok_or_else(|| {
                HostError::classified(HostErrorKind::InvalidRequest, "Image data is missing")
            })?;
            if data.len() as u64 >= crate::images::base64_len(IMAGE_LIMIT) {
                let decoded = decoded_len(data)?;
                if decoded > IMAGE_LIMIT {
                    *block = ContentBlock::Text {
                        text: format!(
                            "[Image omitted: the tool image is {decoded} bytes, above the 16 MiB limit.]"
                        ),
                    };
                    continue;
                }
            }
            let bytes = STANDARD
                .decode(data)
                .map_err(|e| HostError::classified(HostErrorKind::InvalidRequest, e.to_string()))?;
            let accepted = tokio::task::spawn_blocking(move || {
                crate::images::scale_image(std::io::Cursor::new(bytes))
            })
            .await
            .map_err(|e| HostError::failed(e.to_string()))?;
            let Ok(accepted) = accepted else {
                *block = ContentBlock::Text {
                    text: "[Image omitted: the image cannot be sent to this model.]".into(),
                };
                continue;
            };
            let stored = files
                .0
                .put(
                    session.to_owned(),
                    "image".into(),
                    accepted.bytes,
                    cancel.clone(),
                )
                .await
                .map_err(|e| HostError::failed(e.to_string()))?;
            *source = ImageSource {
                source_type: ImageSourceType::File,
                file_id: Some(stored.file_id),
                media_type: accepted.media_type.into(),
                data: None,
                url: None,
            };
        }
    }
    Ok(())
}

/// The bytes base64 `data` decodes to, validated in fixed-size chunks so an
/// image of unknown size is never decoded into one buffer. The validation is
/// [`STANDARD`]'s: `=` only as the final one or two characters, and a final
/// quad whose trailing bits are clear.
fn decoded_len(data: &str) -> Result<u64, HostError> {
    const CHUNK: usize = 64 * 1024;
    let encoded = data.as_bytes();
    let padding = encoded
        .iter()
        .rev()
        .take(2)
        .filter(|byte| **byte == b'=')
        .count();
    if encoded[..encoded.len() - padding].contains(&b'=') {
        return Err(HostError::classified(
            HostErrorKind::InvalidRequest,
            "Invalid base64 padding",
        ));
    }
    let mut buffer = vec![0u8; CHUNK / 4 * 3];
    let mut decoded = 0u64;
    for chunk in encoded.chunks(CHUNK) {
        let written = STANDARD
            .decode_slice(chunk, &mut buffer[..chunk.len() / 4 * 3])
            .map_err(|error| {
                HostError::classified(HostErrorKind::InvalidRequest, error.to_string())
            })?;
        decoded += written as u64;
    }
    Ok(decoded)
}

impl rutis_bridge::session::HostDispatch for SessionFiles {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"put":"async", "import":"async", "read":"async", "register":"async", "owns":"async", "owned_paths":"async"}),
        )
    }
    fn invoke(
        &self,
        method: &str,
        args: rutis_bridge::session::Value,
    ) -> rutis_bridge::session::Reply {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use rutis_bridge::session::{Error, Value};
        #[derive(serde::Deserialize)]
        #[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
        enum Operation {
            Put {
                #[serde(rename = "sessionId")]
                session: String,
                name: String,
                content: String,
            },
            Import {
                #[serde(rename = "sessionId")]
                session: String,
                name: String,
                source: String,
            },
            Read {
                #[serde(rename = "sessionId")]
                session: String,
                #[serde(rename = "fileId")]
                file: String,
                offset: u64,
            },
            Register {
                #[serde(rename = "sessionId")]
                session: String,
                path: String,
            },
            Owns {
                #[serde(rename = "sessionId")]
                session: String,
                path: String,
            },
            OwnedPaths {
                #[serde(rename = "sessionId")]
                session: String,
            },
        }
        let parameters = args.json()?;
        let [mut object] = serde_json::from_value::<[serde_json::Value; 1]>(parameters)
            .map_err(|e| Error::value(e.to_string()))?;
        object
            .as_object_mut()
            .ok_or_else(|| Error::value("Expected one request object"))?
            .insert("method".into(), method.into());
        let operation: Operation =
            serde_json::from_value(object).map_err(|e| Error::value(e.to_string()))?;
        let files = self.0.clone();
        Ok(Value::cancellable_future(async move {
            let cancel = CancellationToken::new();
            let _drop = cancel.clone().drop_guard();
            let result: Result<serde_json::Value, SessionFileError> = async {
                Ok(match operation {
                    Operation::Put {
                        session,
                        name,
                        content,
                    } => {
                        let bytes = STANDARD.decode(content).map_err(|e| SessionFileError {
                            code: SessionFileErrorCode::Invalid,
                            message: e.to_string(),
                        })?;
                        serde_json::json!(files.put(session, name, bytes, cancel).await?)
                    }
                    Operation::Import {
                        session,
                        name,
                        source,
                    } => serde_json::json!(files.import(session, name, source, cancel).await?),
                    Operation::Read {
                        session,
                        file,
                        offset,
                    } => {
                        let bytes = files.read(session, file, offset, cancel).await?;
                        serde_json::json!({"data":STANDARD.encode(&bytes),"eof":bytes.is_empty()})
                    }
                    Operation::Register { session, path } => {
                        files.register(session, path.into()).await?;
                        serde_json::json!({})
                    }
                    Operation::Owns { session, path } => {
                        serde_json::json!({"owned":files.owns(session,path.into()).await?})
                    }
                    Operation::OwnedPaths { session } => {
                        serde_json::json!({"paths":files.owned_paths(session).await?})
                    }
                })
            }
            .await;
            result.map(Value::from).map_err(|error| {
                let code = match error.code {
                    SessionFileErrorCode::NotFound => "not_found",
                    SessionFileErrorCode::Invalid => "invalid",
                    SessionFileErrorCode::TooLarge => "too_large",
                    SessionFileErrorCode::NodeUnavailable => "node_unavailable",
                    SessionFileErrorCode::Cancelled => "cancelled",
                    SessionFileErrorCode::Storage => "storage",
                };
                rutis_bridge::exports::failure("SessionFileError", code, error.message, None)
            })
        }))
    }
}
