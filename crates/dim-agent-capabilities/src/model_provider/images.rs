//! Image blocks become content references per request (Attachments A.1).
//! The rule covers user content and tool results alike.
//! The adopted input and the history are never changed.
use crate::host::{HostError, SessionFiles};
use crate::images::image_reference;
use dim_agent_types::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const NO_IMAGE_INPUT: &str = "[Image omitted: the current model does not accept images.]";
const UNSENDABLE: &str = "[Image omitted: the image cannot be sent to this model.]";

/// Returns the request with image references or placeholders in place of
/// image blocks, sharing messages that contain no images.
pub(super) async fn adapt(
    request: &Arc<ModelRequest>,
    files: Option<&SessionFiles>,
    cancel: &CancellationToken,
) -> Result<Arc<ModelRequest>, HostError> {
    let vision = request
        .capability_snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.capabilities.vision);
    let mut replacements = Vec::new();
    for (index, message) in request.messages.iter().enumerate() {
        if let Some(message) = replace_images(message, request, files, vision, cancel).await? {
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

/// The message with its images replaced, or `None` if it has none.
async fn replace_images(
    message: &Message,
    request: &ModelRequest,
    files: Option<&SessionFiles>,
    vision: bool,
    cancel: &CancellationToken,
) -> Result<Option<Message>, HostError> {
    let MessageContent::Parts(parts) = &message.content else {
        return Ok(None);
    };
    let mut replacements = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let replacement = match part {
            MessagePart::Image { source } => Some(MessagePart::Text {
                provider_metadata: None,
                text: reference_or_placeholder(source, request, files, vision, cancel).await?,
                start_time: None,
                end_time: None,
            }),
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
                    replacements.push((
                        index,
                        ContentBlock::Text {
                            text: reference_or_placeholder(source, request, files, vision, cancel)
                                .await?,
                        },
                    ));
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

async fn reference_or_placeholder(
    source: &ImageSource,
    request: &ModelRequest,
    files: Option<&SessionFiles>,
    vision: bool,
    cancel: &CancellationToken,
) -> Result<String, HostError> {
    crate::context_policy::check_cancel(cancel)?;
    let resolved;
    let source = if source.source_type == ImageSourceType::File {
        resolved = crate::session_files::resolve(request, files, source, cancel).await?;
        &resolved
    } else {
        source
    };
    Ok(image_reference(source)
        .unwrap_or_else(|| if vision { UNSENDABLE } else { NO_IMAGE_INPUT }.into()))
}
