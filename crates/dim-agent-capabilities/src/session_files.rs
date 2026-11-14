use crate::host::{HostError, HostErrorKind, SessionFileErrorCode, SessionFilesHost};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_types::{
    ContentBlock, ImageSource, ImageSourceType, MessageContent, MessagePart, ModelRequest,
    ToolContent,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Materializes a durable image at an external protocol boundary.
pub async fn image(
    files: &SessionFilesHost,
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
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(HostError::classified(
                HostErrorKind::InvalidRequest,
                "Image file exceeds its storage limit",
            ));
        }
    }
    let scaled = tokio::task::spawn_blocking(move || {
        crate::images::scale_image(std::io::Cursor::new(bytes))
    })
    .await
    .map_err(|e| HostError::classified(HostErrorKind::InvalidRequest, format!("{e:?}")))?
    .map_err(|e| HostError::classified(HostErrorKind::InvalidRequest, format!("{e:?}")))?;
    Ok(ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: scaled.media_type.into(),
        data: Some(STANDARD.encode(scaled.bytes)),
        url: None,
        file_id: None,
    })
}

/// Resolves only the images selected for this model request; the stored request stays immutable.
pub async fn materialize(
    request: &Arc<ModelRequest>,
    files: Option<&SessionFilesHost>,
    cancel: &CancellationToken,
) -> Result<Arc<ModelRequest>, HostError> {
    let mut changed = None;
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

async fn resolve(
    request: &ModelRequest,
    files: Option<&SessionFilesHost>,
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
    files: &SessionFilesHost,
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
            if data.len() as u64 > crate::images::base64_len(16 * 1024 * 1024) {
                return Err(HostError::classified(
                    HostErrorKind::InvalidRequest,
                    "Tool image exceeds 16 MiB",
                ));
            }
            let bytes = STANDARD
                .decode(data)
                .map_err(|e| HostError::classified(HostErrorKind::InvalidRequest, e.to_string()))?;
            let accepted = tokio::task::spawn_blocking(move || {
                crate::images::scale_image(std::io::Cursor::new(bytes))
            })
            .await
            .map_err(|e| HostError::failed(e.to_string()))?
            .map_err(|e| {
                HostError::classified(
                    HostErrorKind::InvalidRequest,
                    format!("Invalid tool image: {e:?}"),
                )
            })?;
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
