mod names;
mod native;
mod reasoning;

#[cfg(test)]
mod tests;

pub(crate) use names::ToolNames;

use crate::config::ProviderKind;
use aimux_core::{
    content::ContentPart,
    language_model_message::LanguageModelPromptMessage,
    message::Role as AimuxRole,
    options::{CallOptions, ToolChoice},
    tool::{FunctionTool, Tool},
};
use dim_agent_capabilities::host::HostError;
use dim_agent_types::{
    ContentBlock, ContentMetadata, ImageSource, ImageSourceType, MessageContent, MessagePart,
    ModelRequest, Role, SystemPrompt, SystemPromptPart, ToolContent,
};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) struct PreparedRequest {
    pub options: CallOptions,
    pub tool_names: ToolNames,
}

/// The tool result's text where an image moved to the following user message.
const TOOL_IMAGE_MARKER: &str = "[Image: sent in the following user message as tool output.]";
/// Names the tool call an image came from, before that image.
const TOOL_IMAGE_LABEL: &str = "Tool output from tool call";

pub(crate) fn convert(
    request: &ModelRequest,
    kind: &ProviderKind,
    preset: Option<&str>,
) -> Result<PreparedRequest, HostError> {
    let mut tool_names = ToolNames::default();
    let mut prompt = Vec::new();
    match request.system.as_deref() {
        Some(SystemPrompt::Text(text)) => {
            prompt.push(message(AimuxRole::System, vec![ContentPart::text(text)]))
        }
        Some(SystemPrompt::Parts(parts)) => {
            for (
                index,
                SystemPromptPart::Text {
                    text,
                    cache_control,
                },
            ) in parts.iter().enumerate()
            {
                if cache_control.is_some() && *kind != ProviderKind::Anthropic {
                    return Err(error(
                        request,
                        &format!("system[{index}].cache_control"),
                        "cache control is only supported by Anthropic",
                    ));
                }
                let part = ContentPart::Text {
                    text: text.clone(),
                    provider_options: cache_control
                        .map(|control| json!({"anthropic":{"cacheControl":control}})),
                };
                prompt.push(message(AimuxRole::System, vec![part]));
            }
        }
        None => {}
    }
    let tools = request
        .tools
        .as_ref()
        .filter(|tools| !tools.is_empty())
        .map(|tools| {
            tools
                .iter()
                .enumerate()
                .map(|(index, tool)| {
                    let name = tool_names.register(&tool.name).map_err(|reason| {
                        error(request, &format!("tools[{index}].name"), &reason)
                    })?;
                    let schema = serde_json::to_value(&tool.input_schema).map_err(|reason| {
                        error(
                            request,
                            &format!("tools[{index}].inputSchema"),
                            &reason.to_string(),
                        )
                    })?;
                    Ok(Tool::Function(
                        FunctionTool::new(name, schema).with_description(&tool.description),
                    ))
                })
                .collect::<Result<Vec<_>, HostError>>()
        })
        .transpose()?;

    let mut calls = HashMap::new();
    // Images a tool message cannot carry, in tool-call order; they become one
    // user message once the step's tool messages end.
    let mut following = Vec::new();
    for (index, source) in request.messages.iter().enumerate() {
        if source.role != Role::ToolResult && !following.is_empty() {
            prompt.push(message(AimuxRole::User, std::mem::take(&mut following)));
        }
        let path = format!("messages[{index}].content");
        let content = match &source.content {
            MessageContent::Text(text) if source.role != Role::ToolResult => {
                vec![ContentPart::text(text)]
            }
            MessageContent::Text(_) => {
                return Err(error(request, &path, "tool results require a tool call ID"));
            }
            MessageContent::Parts(parts) => {
                let mut content = Vec::with_capacity(parts.len());
                for (part_index, part) in parts.iter().enumerate() {
                    let path = format!("{path}[{part_index}]");
                    let mapped = match (source.role, part) {
                        (Role::User | Role::Assistant, MessagePart::Text { text, .. }) => {
                            Ok(ContentPart::text(text))
                        }
                        (Role::Assistant, MessagePart::Thinking { thinking, .. }) => {
                            Ok(ContentPart::reasoning(thinking))
                        }
                        (Role::User, MessagePart::Image { source }) => {
                            image_part(request, &path, source)
                        }
                        (
                            Role::Assistant,
                            MessagePart::ToolUse {
                                id, name, input, ..
                            },
                        ) => {
                            let name = tool_names.register(name).map_err(|reason| {
                                error(request, &format!("{path}.name"), &reason)
                            })?;
                            calls.insert(id.as_str(), name.clone());
                            Ok(ContentPart::tool_call(
                                id,
                                name,
                                Value::Object(input.clone()),
                            ))
                        }
                        (
                            Role::ToolResult,
                            MessagePart::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                ..
                            },
                        ) => {
                            let name = calls.get(tool_use_id.as_str()).ok_or_else(|| {
                                error(
                                    request,
                                    &format!("{path}.tool_use_id"),
                                    "no preceding tool call",
                                )
                            })?;
                            let result = tool_result(
                                request,
                                kind,
                                &path,
                                tool_use_id,
                                content,
                                *is_error == Some(true),
                                &mut following,
                            )?;
                            Ok(ContentPart::ToolResult {
                                tool_call_id: tool_use_id.clone(),
                                result: if *kind == ProviderKind::Anthropic
                                    && *is_error == Some(true)
                                    && !matches!(result, Value::Object(_))
                                {
                                    json!({"type":"error", "value":result})
                                } else {
                                    result
                                },
                                tool_name: Some(name.clone()),
                                is_error: *is_error,
                                preliminary: None,
                                dynamic: None,
                                provider_options: None,
                            })
                        }
                        _ => Err(error(
                            request,
                            &path,
                            "content cannot be represented for this message role",
                        )),
                    };
                    let mut mapped = mapped?;
                    if continuation(request, kind, &path, part, &mut mapped)? {
                        content.push(mapped);
                    }
                }
                content
            }
        };
        let role = match source.role {
            Role::User => AimuxRole::User,
            Role::Assistant => AimuxRole::Assistant,
            Role::ToolResult => AimuxRole::Tool,
        };
        prompt.push(message(role, content));
    }
    if !following.is_empty() {
        prompt.push(message(AimuxRole::User, following));
    }

    let mut options = CallOptions {
        tool_choice: if tools.is_some() {
            ToolChoice::Auto
        } else {
            ToolChoice::None
        },
        tools,
        max_retries: Some(0),
        ..CallOptions::new(prompt)
    };
    if let Some(config) = &request.config {
        options.temperature = config.temperature;
        options.top_p = config.top_p;
        options.top_k = config.top_k;
        options.stop_sequences = config.stop_sequences.clone();
        options.max_output_tokens = config
            .max_tokens
            .map(|value| {
                if !value.is_finite()
                    || value.fract() != 0.0
                    || !(1.0..=f64::from(u32::MAX)).contains(&value)
                {
                    Err(error(
                        request,
                        "config.maxTokens",
                        "must be an integer in 1..=4294967295 for aimux",
                    ))
                } else {
                    Ok(value as u32)
                }
            })
            .transpose()?;
    }
    reasoning::apply(
        request,
        request.config.as_ref().unwrap_or(&Default::default()),
        kind,
        preset,
        &mut options,
    )?;
    if *kind == ProviderKind::Anthropic
        && options.prompt.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Reasoning { .. }))
        })
        && !options
            .provider_options
            .as_ref()
            .and_then(|p| p.get("anthropic"))
            .and_then(|p| p.get("thinking"))
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "enabled" | "adaptive"))
    {
        return Err(error(
            request,
            "config",
            "Anthropic thinking continuation requires enabled or adaptive thinking",
        ));
    }
    Ok(PreparedRequest {
        options,
        tool_names,
    })
}

fn message(role: AimuxRole, content: Vec<ContentPart>) -> LanguageModelPromptMessage {
    LanguageModelPromptMessage {
        role,
        content,
        provider_options: None,
    }
}

/// An image block as the protocols' user messages carry it.
fn image_part(
    request: &ModelRequest,
    path: &str,
    source: &ImageSource,
) -> Result<ContentPart, HostError> {
    match source.source_type {
        ImageSourceType::Base64 => {
            let data = source.data.as_ref().ok_or_else(|| {
                error(
                    request,
                    &format!("{path}.source.data"),
                    "base64 image has no data",
                )
            })?;
            Ok(ContentPart::file_base64(data, &source.media_type))
        }
        ImageSourceType::Url => {
            let url = source.url.as_ref().ok_or_else(|| {
                error(
                    request,
                    &format!("{path}.source.url"),
                    "URL image has no URL",
                )
            })?;
            Ok(ContentPart::file_url(url, &source.media_type))
        }
    }
}

/// A tool result's `output` value. Protocols whose tool message carries an
/// image keep it inside the result; the others mark its place in the text and
/// put the image in `following` (one user message after the step's tools).
fn tool_result(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    tool_use_id: &str,
    content: &ToolContent,
    is_error: bool,
    following: &mut Vec<ContentPart>,
) -> Result<Value, HostError> {
    let blocks = match content {
        ToolContent::Text(text) => return Ok(Value::String(text.clone())),
        ToolContent::Blocks(blocks) => blocks,
    };
    // A result without images keeps its existing text-only form.
    if !blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }))
    {
        let mut text = String::new();
        for block in blocks {
            if let ContentBlock::Text { text: block } = block {
                text.push_str(block);
            }
        }
        return Ok(Value::String(text));
    }
    let inside = matches!(kind, ProviderKind::Anthropic | ProviderKind::Bedrock);
    let mut result = Vec::with_capacity(blocks.len());
    let mut text = String::new();
    for block in blocks {
        let marker = match block {
            ContentBlock::Text { text: block } => {
                if inside {
                    result.push(json!({"type":"text","text":block}));
                } else {
                    text.push_str(block);
                }
                continue;
            }
            ContentBlock::Image { source } => match (inside, base64_data(source)) {
                (true, Some(data)) => {
                    result.push(inside_image(kind, source, data));
                    continue;
                }
                _ => {
                    following.push(ContentPart::text(format!(
                        "{TOOL_IMAGE_LABEL} {tool_use_id}: the image after this text is that call's result."
                    )));
                    following.push(image_part(request, path, source)?);
                    TOOL_IMAGE_MARKER
                }
            },
        };
        if inside {
            result.push(json!({"type":"text","text":marker}));
        } else {
            text.push_str(marker);
        }
    }
    if inside {
        // The Anthropic converter takes the error flag from the output's type
        // (`resolve_tool_result_output`); Bedrock reads the part's flag, so it
        // keeps `content`.
        let output = match kind {
            ProviderKind::Anthropic if is_error => "error",
            _ => "content",
        };
        Ok(json!({"type": output, "value": result}))
    } else {
        Ok(Value::String(text))
    }
}

fn base64_data(source: &ImageSource) -> Option<&str> {
    if source.source_type != ImageSourceType::Base64 {
        return None;
    }
    source.data.as_deref()
}

/// An image inside a tool result, in the form that protocol's converter reads.
fn inside_image(kind: &ProviderKind, source: &ImageSource, data: &str) -> Value {
    match kind {
        ProviderKind::Anthropic => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": source.media_type,
                "data": data,
            },
        }),
        _ => json!({
            "type": "file",
            "mediaType": source.media_type,
            "data": {"type": "data", "data": data},
        }),
    }
}

fn error(request: &ModelRequest, path: &str, reason: &str) -> HostError {
    HostError::failed(format!(
        "{}/{}: {path}: {reason}",
        request.provider_id, request.model_id
    ))
}

/// Attaches the native continuation fields a stored part carries, and says
/// whether the part belongs in the request. A thinking part without
/// continuation data for this protocol stays in the Timeline and in display,
/// but is not sent back: the endpoint cannot continue it, and an empty or
/// invented signature is rejected by a strict Anthropic implementation.
fn continuation(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    part: &MessagePart,
    native: &mut ContentPart,
) -> Result<bool, HostError> {
    let thinking = matches!(part, MessagePart::Thinking { .. });
    let metadata = match part {
        MessagePart::Text {
            provider_metadata, ..
        }
        | MessagePart::Thinking {
            provider_metadata, ..
        }
        | MessagePart::ToolUse {
            provider_metadata, ..
        } => provider_metadata.as_ref(),
        _ => None,
    };
    let Some(metadata) = metadata else {
        if thinking
            && matches!(
                kind,
                ProviderKind::Anthropic
                    | ProviderKind::OpenaiResponses
                    | ProviderKind::XaiResponses
            )
        {
            warn_dropped(kind);
            return Ok(false);
        }
        if thinking && *kind == ProviderKind::Bedrock {
            return Err(error(
                request,
                path,
                "thinking requires native continuation metadata",
            ));
        }
        return Ok(true);
    };
    let options = match (kind, metadata) {
        (ProviderKind::Bedrock, ContentMetadata::Bedrock { signature }) if thinking => {
            match signature {
                Some(signature) => json!({"bedrock":{"signature":signature}}),
                None => json!({"bedrock":{}}),
            }
        }
        (ProviderKind::Bedrock, ContentMetadata::BedrockRedacted { data }) if thinking => {
            json!({"bedrock":{"redactedData":data}})
        }
        (ProviderKind::Anthropic, ContentMetadata::Anthropic { signature }) if thinking => {
            if signature.is_empty() {
                warn_dropped(kind);
                return Ok(false);
            }
            json!({"anthropic":{"signature":signature}})
        }
        (ProviderKind::Anthropic, ContentMetadata::AnthropicRedacted { data }) if thinking => {
            if data.is_empty() {
                warn_dropped(kind);
                return Ok(false);
            }
            json!({"anthropic":{"redactedData":data}})
        }
        (ProviderKind::Gemini, ContentMetadata::Gemini { thought_signature }) => {
            json!({"google":{"thoughtSignature":thought_signature}})
        }
        (
            ProviderKind::XaiResponses,
            ContentMetadata::XaiResponses {
                item_id,
                encrypted_content,
            },
        ) => {
            if thinking && encrypted_content.as_ref().is_none_or(String::is_empty) {
                warn_dropped(kind);
                return Ok(false);
            }
            let mut options = serde_json::Map::from_iter([("itemId".into(), json!(item_id))]);
            if let Some(encrypted) = encrypted_content {
                options.insert("reasoningEncryptedContent".into(), json!(encrypted));
            }
            json!({"xai": options})
        }
        (
            ProviderKind::OpenaiResponses,
            ContentMetadata::OpenaiResponses {
                item_id,
                encrypted_content,
                summary_index,
                phase,
            },
        ) => {
            if thinking && encrypted_content.as_ref().is_none_or(String::is_empty) {
                warn_dropped(kind);
                return Ok(false);
            }
            let mut options = serde_json::Map::from_iter([("itemId".into(), json!(item_id))]);
            if let Some(encrypted) = encrypted_content {
                options.insert("reasoningEncryptedContent".into(), json!(encrypted));
            }
            if let Some(index) = summary_index {
                options.insert("summaryIndex".into(), json!(index));
            }
            if let Some(phase) = phase {
                options.insert("phase".into(), json!(phase));
            }
            json!({"openai":options})
        }
        _ => {
            return Err(error(
                request,
                &format!("{path}.providerMetadata"),
                "continuation metadata belongs to a different protocol or content kind",
            ));
        }
    };
    match native {
        ContentPart::Text {
            provider_options, ..
        }
        | ContentPart::Reasoning {
            provider_options, ..
        }
        | ContentPart::ToolCall {
            provider_options, ..
        } => *provider_options = Some(options),
        _ => unreachable!(),
    }
    Ok(true)
}

fn warn_dropped(kind: &ProviderKind) {
    tracing::warn!(
        protocol = match kind {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenaiResponses => "openai-responses",
            ProviderKind::XaiResponses => "xai-responses",
            ProviderKind::OpenaiChat => "openai-chat",
            ProviderKind::OpenaiCompatible => "openai-compatible",
            ProviderKind::Xai => "xai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Bedrock => "bedrock",
        },
        "thinking without continuation data was left out of the request"
    );
}
