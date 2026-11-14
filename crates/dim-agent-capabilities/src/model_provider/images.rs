//! Image blocks in a request the model cannot take become text, per request
//! (Attachments A.1). The rule covers user content and tool results alike.
//! The adopted input and the history are never changed.
use base64::Engine;
use dim_agent_types::*;
use std::sync::Arc;

const NO_IMAGE_INPUT: &str = "[Image omitted: the current model does not accept images.]";
const UNSENDABLE: &str = "[Image omitted: the image cannot be sent to this model.]";

/// Image formats every adapter accepts (feature list).
const FORMATS: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Returns the request to send: the same request when every image is sent as
/// is, otherwise a copy whose unsendable images are text placeholders.
pub(super) fn adapt(request: &Arc<ModelRequest>) -> Arc<ModelRequest> {
    // Unknown capability: the image is sent and the provider decides (A4).
    let vision = request
        .capability_snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.capabilities.vision);
    let replacements: Vec<_> = request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            replace_images(message, vision).map(|message| (index, Arc::new(message)))
        })
        .collect();
    if replacements.is_empty() {
        return request.clone();
    }
    Arc::new(ModelRequest {
        provider_id: request.provider_id.clone(),
        model_id: request.model_id.clone(),
        capability_snapshot: request.capability_snapshot.clone(),
        messages: replace_selected(&request.messages, replacements),
        tools: request.tools.clone(),
        system: request.system.clone(),
        config: request.config.clone(),
        metadata: request.metadata.clone(),
    })
}

/// The message with its unsendable images replaced, or `None` if it has none.
fn replace_images(message: &Message, vision: bool) -> Option<Message> {
    let MessageContent::Parts(parts) = &message.content else {
        return None;
    };
    let replacements: Vec<_> = parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let replacement = match part {
                MessagePart::Image { source } => {
                    placeholder(source, vision).map(|text| MessagePart::Text {
                        provider_metadata: None,
                        text: text.into(),
                        start_time: None,
                        end_time: None,
                    })
                }
                MessagePart::ToolResult {
                    tool_use_id,
                    content: ToolContent::Blocks(blocks),
                    structured_content,
                    is_error,
                } => {
                    let replacements: Vec<_> = blocks
                        .iter()
                        .enumerate()
                        .filter_map(|(index, block)| {
                            let ContentBlock::Image { source } = block else {
                                return None;
                            };
                            placeholder(source, vision)
                                .map(|text| (index, ContentBlock::Text { text: text.into() }))
                        })
                        .collect();
                    (!replacements.is_empty()).then(|| MessagePart::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: ToolContent::Blocks(replace_selected(blocks, replacements)),
                        structured_content: structured_content.clone(),
                        is_error: *is_error,
                    })
                }
                _ => None,
            };
            replacement.map(|part| (index, part))
        })
        .collect();
    (!replacements.is_empty()).then(|| Message {
        role: message.role,
        content: MessageContent::Parts(replace_selected(parts, replacements)),
    })
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

/// The placeholder an image becomes, or `None` when the image is sent as is.
fn placeholder(source: &ImageSource, vision: bool) -> Option<&'static str> {
    if !vision {
        Some(NO_IMAGE_INPUT)
    } else if !sendable(source) {
        Some(UNSENDABLE)
    } else {
        None
    }
}

/// A URL source, undecodable data or a format adapters do not accept (A10).
fn sendable(source: &ImageSource) -> bool {
    (source.source_type == ImageSourceType::File
        && source.file_id.as_ref().is_some_and(|id| !id.is_empty()))
        || source.source_type == ImageSourceType::Base64
            && FORMATS.contains(&source.media_type.as_str())
            && source.data.as_deref().is_some_and(|data| {
                !data.is_empty()
                    && base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .is_ok()
            })
}
