//! Image blocks in a request the model cannot take become content references
//! per request (Attachments A.1). The rule covers user content and tool
//! results alike. A vision-capable model receives a sendable image as it is
//! when the destination takes its format. The adopted input and the history
//! are never changed.
use crate::host::{HostError, SessionFiles};
use crate::images::{
    REQUEST_IMAGE_FORMATS, image_reference, image_reference_or_placeholder, takes_image_format,
};
use crate::session_files::check_image_budget;
use base64::Engine;
use dim_agent_types::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const NO_IMAGE_INPUT: &str = "[Image omitted: the current model does not accept images.]";

/// What one image becomes in the request copy.
enum Routed {
    /// The source is already the form this model takes.
    Keep,
    /// The file-backed source resolved to the bytes this model takes.
    Resolved(ImageSource),
    /// The reference or placeholder that replaces the image.
    Text(String),
}

/// The destination a request's images are judged by: the protocol it serves,
/// and the vendor package a compatible destination builds by name.
#[derive(Clone, Copy, Default)]
pub(super) struct ImageDestination<'a> {
    pub(super) protocol: Option<ModelProtocol>,
    pub(super) preset: Option<&'a str>,
}

/// Returns the request to send: the same request when every image is sent as
/// is, otherwise a copy whose images a model cannot take become references or
/// placeholders. Messages without images stay shared. `destination` is the
/// resolved destination, whose fields are `None` when it is not a provider
/// protocol. Each image the copy sends as pixels joins the encoded-image
/// budget, so a request over it fails before the rest of the images are read.
pub(super) async fn adapt(
    request: &Arc<ModelRequest>,
    files: Option<&SessionFiles>,
    destination: ImageDestination<'_>,
    cancel: &CancellationToken,
) -> Result<Arc<ModelRequest>, HostError> {
    // Unknown capability: the image is sent and the provider decides (A4).
    let vision = request
        .capability_snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.capabilities.vision);
    let mut encoded = 0;
    let mut replacements = Vec::new();
    for (index, message) in request.messages.iter().enumerate() {
        if let Some(message) = replace_images(
            message,
            request,
            files,
            vision,
            destination,
            cancel,
            &mut encoded,
        )
        .await?
        {
            replacements.push((index, Arc::new(message)));
        }
    }
    if replacements.is_empty() {
        return Ok(request.clone());
    }
    Ok(Arc::new(ModelRequest {
        provider_id: request.provider_id.clone(),
        model_id: request.model_id.clone(),
        capability_snapshot: request.capability_snapshot.clone(),
        messages: replace_selected(&request.messages, replacements),
        tools: request.tools.clone(),
        system: request.system.clone(),
        config: request.config.clone(),
        metadata: request.metadata.clone(),
    }))
}

/// The message with its images routed, or `None` if every image is sent as it
/// is.
async fn replace_images(
    message: &Message,
    request: &ModelRequest,
    files: Option<&SessionFiles>,
    vision: bool,
    destination: ImageDestination<'_>,
    cancel: &CancellationToken,
    encoded: &mut usize,
) -> Result<Option<Message>, HostError> {
    let MessageContent::Parts(parts) = &message.content else {
        return Ok(None);
    };
    let mut replacements = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let replacement = match part {
            MessagePart::Image { source } => {
                match route(source, request, files, vision, destination, cancel, encoded).await? {
                    Routed::Keep => None,
                    Routed::Resolved(source) => Some(MessagePart::Image { source }),
                    Routed::Text(text) => Some(MessagePart::Text {
                        provider_metadata: None,
                        text,
                        start_time: None,
                        end_time: None,
                    }),
                }
            }
            MessagePart::ToolResult {
                tool_use_id,
                content: ToolContent::Blocks(blocks),
                structured_content,
                is_error,
            } => {
                let mut replacements = Vec::new();
                for (index, block) in blocks.iter().enumerate() {
                    let ContentBlock::Image { source } = block else {
                        continue;
                    };
                    match route(source, request, files, vision, destination, cancel, encoded)
                        .await?
                    {
                        Routed::Keep => {}
                        Routed::Resolved(source) => {
                            replacements.push((index, ContentBlock::Image { source }));
                        }
                        Routed::Text(text) => {
                            replacements.push((index, ContentBlock::Text { text }));
                        }
                    }
                }
                (!replacements.is_empty()).then(|| MessagePart::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: ToolContent::Blocks(replace_selected(blocks, replacements)),
                    structured_content: structured_content.clone(),
                    is_error: *is_error,
                })
            }
            _ => None,
        };
        if let Some(part) = replacement {
            replacements.push((index, part));
        }
    }
    Ok((!replacements.is_empty()).then(|| Message {
        role: message.role,
        content: MessageContent::Parts(replace_selected(parts, replacements)),
    }))
}

fn replace_selected<T: Clone>(values: &[T], replacements: Vec<(usize, T)>) -> Vec<T> {
    let mut replacements = replacements.into_iter().peekable();
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if replacements.peek().is_some_and(|(at, _)| *at == index) {
                replacements.next().unwrap().1
            } else {
                value.clone()
            }
        })
        .collect()
}

/// The form one image takes for this model: sent as it is when the model
/// takes images, the image is sendable and the destination takes its format;
/// else its reference or placeholder. A file-backed source is validated after
/// it is resolved too: an empty owned file is not an image even though it has
/// an id and a supported format.
async fn route(
    source: &ImageSource,
    request: &ModelRequest,
    files: Option<&SessionFiles>,
    vision: bool,
    destination: ImageDestination<'_>,
    cancel: &CancellationToken,
    encoded: &mut usize,
) -> Result<Routed, HostError> {
    crate::context_policy::check_cancel(cancel)?;
    if source.source_type == ImageSourceType::File && sendable(source) {
        let resolved = crate::session_files::resolve(request, files, source, cancel).await?;
        if vision
            && sendable(&resolved)
            && takes_image_format(
                destination.protocol,
                destination.preset,
                &resolved.media_type,
            )
        {
            add_encoded_image(encoded, &resolved)?;
            return Ok(Routed::Resolved(resolved));
        }
        return Ok(Routed::Text(if vision {
            image_reference_or_placeholder(&resolved)
        } else {
            image_reference(&resolved).unwrap_or_else(|| NO_IMAGE_INPUT.into())
        }));
    }
    if vision
        && sendable(source)
        && takes_image_format(destination.protocol, destination.preset, &source.media_type)
    {
        add_encoded_image(encoded, source)?;
        return Ok(Routed::Keep);
    }
    Ok(Routed::Text(if vision {
        image_reference_or_placeholder(source)
    } else {
        image_reference(source).unwrap_or_else(|| NO_IMAGE_INPUT.into())
    }))
}

/// Adds one image the request sends as pixels to the encoded-image budget and
/// fails when the request would exceed it.
fn add_encoded_image(encoded: &mut usize, source: &ImageSource) -> Result<(), HostError> {
    *encoded += source.data.as_ref().map_or(0, String::len);
    check_image_budget(*encoded)
}

/// A URL source, a file without an id, undecodable data or a format adapters
/// do not accept (A10).
fn sendable(source: &ImageSource) -> bool {
    REQUEST_IMAGE_FORMATS.contains(&source.media_type.as_str())
        && match source.source_type {
            ImageSourceType::File => source.file_id.as_ref().is_some_and(|id| !id.is_empty()),
            ImageSourceType::Base64 => source.data.as_deref().is_some_and(|data| {
                !data.is_empty()
                    && base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .is_ok()
            }),
            ImageSourceType::Url => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{SessionFileError, SessionFilesService, StoredSessionFile};
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Serves one 100-byte chunk per file, more than the test target's whole
    /// budget, and counts the files it opened.
    struct Files {
        opened: AtomicUsize,
    }

    impl SessionFilesService for Files {
        fn read(
            &self,
            _: String,
            _: String,
            offset: u64,
            _: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, SessionFileError>> + Send + '_>> {
            if offset == 0 {
                self.opened.fetch_add(1, Ordering::SeqCst);
            }
            Box::pin(async move {
                Ok(if offset == 0 {
                    vec![b'x'; 100]
                } else {
                    Vec::new()
                })
            })
        }

        fn put(
            &self,
            _: String,
            _: String,
            _: Vec<u8>,
            _: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<StoredSessionFile, SessionFileError>> + Send + '_>>
        {
            unimplemented!("the test only reads files")
        }

        fn register(
            &self,
            _: String,
            _: PathBuf,
        ) -> Pin<Box<dyn Future<Output = Result<(), SessionFileError>> + Send + '_>> {
            unimplemented!("the test only reads files")
        }

        fn owns(
            &self,
            _: String,
            _: PathBuf,
        ) -> Pin<Box<dyn Future<Output = Result<bool, SessionFileError>> + Send + '_>> {
            unimplemented!("the test only reads files")
        }

        fn owned_paths(
            &self,
            _: String,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<PathBuf>, SessionFileError>> + Send + '_>>
        {
            unimplemented!("the test only reads files")
        }
    }

    fn file_image(id: &str) -> ImageSource {
        ImageSource {
            source_type: ImageSourceType::File,
            media_type: "image/png".into(),
            data: None,
            url: None,
            file_id: Some(id.into()),
        }
    }

    /// The test target carries a small cap so the assertion reaches it without
    /// allocating a production-sized request.
    #[tokio::test]
    async fn the_encoded_image_budget_stops_adaptation_before_the_remaining_images() {
        let opened = Arc::new(Files {
            opened: AtomicUsize::new(0),
        });
        let files = SessionFiles(opened.clone());
        let mut request = ModelRequest::new(
            "provider",
            "model",
            vec![Message {
                role: Role::User,
                content: MessageContent::Parts(vec![
                    MessagePart::Image {
                        source: file_image("first"),
                    },
                    MessagePart::Image {
                        source: file_image("second"),
                    },
                ]),
            }],
        );
        request.metadata = Some(RequestMetadata {
            session_id: Some("owner".into()),
            ..Default::default()
        });
        let error = adapt(
            &Arc::new(request),
            Some(&files),
            ImageDestination::default(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the first image exceeds the encoded-image budget");
        assert!(
            error.to_string().contains("Model request images exceed"),
            "the failure names the budget: {error}"
        );
        assert_eq!(
            opened.opened.load(Ordering::SeqCst),
            1,
            "adaptation stops before the remaining image is read"
        );
    }
}
