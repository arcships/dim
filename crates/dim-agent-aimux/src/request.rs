mod names;
mod native;
mod reasoning;

#[cfg(test)]
mod tests;

pub(crate) use names::ToolNames;

use crate::config::ProviderKind;
use aimux_core::{
    language_model_message::{
        AssistantPart, FilePart, LanguageModelMessage, ReasoningPart, TextPart, ToolCallPart,
        ToolPart, ToolResultContent, ToolResultOutput, ToolResultPart, UserPart,
    },
    options::{CallOptions, LanguageModelV4ToolChoice},
    shared::{FileBytes, FileData, SharedProviderOptions, provider_namespace},
    tool::{FunctionTool, Tool},
};
use dim_agent_capabilities::host::{HostError, HostErrorKind};
use dim_agent_types::{
    ContentBlock, ContentMetadata, ImageSource, ImageSourceType, MessageContent, MessagePart,
    ModelRequest, Role, SystemPrompt, SystemPromptPart, ToolContent,
};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) struct PreparedRequest {
    pub options: CallOptions,
    pub tool_names: ToolNames,
    pub recording: Option<crate::recording::Capture>,
}

/// The tool result's text where an image moved to the following user message.
const TOOL_IMAGE_MARKER: &str = "[Image: sent in the following user message as tool output.]";
/// Names the tool call an image came from, before that image.
const TOOL_IMAGE_LABEL: &str = "Tool output from tool call";

/// Anthropic's limit on cache breakpoints in one request.
const ANTHROPIC_CACHE_BREAKPOINTS: usize = 4;

/// The breakpoint this port marks on the System and the conversation tail: the
/// API's five-minute lifetime, refreshed on each use.
fn cache_breakpoint() -> SharedProviderOptions {
    provider_namespace("anthropic", json!({"cacheControl": {"type": "ephemeral"}}))
        .expect("a cache breakpoint is an object")
}

pub(crate) fn convert(
    request: &ModelRequest,
    kind: &ProviderKind,
    preset: Option<&str>,
) -> Result<PreparedRequest, HostError> {
    let mut tool_names = ToolNames::default();
    let mut prompt = Vec::new();
    let mut cache_breakpoints = 0;
    match request.system.as_deref() {
        Some(SystemPrompt::Text(text)) => {
            let provider_options = (*kind == ProviderKind::Anthropic).then(cache_breakpoint);
            cache_breakpoints = usize::from(provider_options.is_some());
            prompt.push(LanguageModelMessage::System {
                content: text.clone(),
                provider_options,
            });
        }
        Some(SystemPrompt::Parts(parts)) => {
            let explicit = parts
                .iter()
                .filter(|part| {
                    matches!(
                        part,
                        SystemPromptPart::Text {
                            cache_control: Some(_),
                            ..
                        }
                    )
                })
                .count();
            let implicit = *kind == ProviderKind::Anthropic && explicit == 0 && !parts.is_empty();
            cache_breakpoints = explicit + usize::from(implicit);
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
                let provider_options = if let Some(control) = cache_control {
                    Some(
                        provider_namespace("anthropic", json!({"cacheControl": control}))
                            .expect("a cache control value is an object"),
                    )
                } else if implicit && index + 1 == parts.len() {
                    Some(cache_breakpoint())
                } else {
                    None
                };
                prompt.push(LanguageModelMessage::System {
                    content: text.clone(),
                    provider_options,
                });
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
            prompt.push(LanguageModelMessage::User {
                content: std::mem::take(&mut following),
                provider_options: None,
            });
        }
        let path = format!("messages[{index}].content");
        let message = match source.role {
            Role::User => user_message(request, kind, &path, &source.content)?,
            Role::Assistant => assistant_message(
                request,
                kind,
                &path,
                &source.content,
                &mut tool_names,
                &mut calls,
            )?,
            Role::ToolResult => tool_message(
                request,
                kind,
                &path,
                &source.content,
                &calls,
                &mut following,
            )?,
        };
        prompt.push(message);
    }
    if !following.is_empty() {
        prompt.push(LanguageModelMessage::User {
            content: following,
            provider_options: None,
        });
    }
    if *kind == ProviderKind::Anthropic
        && cache_breakpoints < ANTHROPIC_CACHE_BREAKPOINTS
        && let Some(last) = prompt.last_mut()
    {
        match last {
            LanguageModelMessage::User {
                provider_options, ..
            }
            | LanguageModelMessage::Tool {
                provider_options, ..
            } => *provider_options = Some(cache_breakpoint()),
            _ => {}
        }
    }

    let mut options = CallOptions {
        tool_choice: Some(if tools.is_some() {
            LanguageModelV4ToolChoice::Auto
        } else {
            LanguageModelV4ToolChoice::None
        }),
        tools,
        strict_stream: Some(true),
        ..CallOptions::new(prompt)
    };
    if let Some(config) = &request.config {
        options.temperature = config.temperature;
        options.top_p = config.top_p;
        options.top_k = config.top_k;
        options.stop_sequences = config.stop_sequences.clone();
        // The xAI Chat protocol is the generic compatible one, whose own
        // token-limit key is `max_tokens`. The API documents
        // `max_completion_tokens`, written through the provider namespace
        // after `reasoning::apply` below, next to `top_k`.
        if *kind != ProviderKind::Xai {
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
        } else if let Some(value) = config.max_tokens
            && (!value.is_finite()
                || value.fract() != 0.0
                || !(1.0..=f64::from(u32::MAX)).contains(&value))
        {
            return Err(error(
                request,
                "config.maxTokens",
                "must be an integer in 1..=4294967295 for aimux",
            ));
        }
    }
    reasoning::apply(
        request,
        request.config.as_ref().unwrap_or(&Default::default()),
        kind,
        preset,
        &mut options,
    )?;
    // The OpenAI-compatible Chat model consumes the fields of its generic
    // schema and passes any other namespace field to the request body, which
    // is how the wire keeps `top_k`: its own `topK` switch stays off for the
    // generic endpoint. The OpenAI Chat model reads `top_k` from the request.
    if *kind == ProviderKind::OpenaiCompatible
        && let Some(top_k) = options.top_k.take()
    {
        options
            .provider_options
            .get_or_insert_default()
            .entry(preset.unwrap_or("openai").into())
            .or_default()
            .insert("top_k".into(), json!(top_k));
    }
    // A model the tag classifies as a reasoning model takes the request rules
    // main applied on both Chat protocols. The OpenAI Chat model applies the
    // token key and the system role itself; the compatible one has no setting
    // for the system role, so the prompt carries it.
    if matches!(
        kind,
        ProviderKind::OpenaiChat | ProviderKind::OpenaiCompatible
    ) {
        let capabilities =
            aimux_providers::openai::language_model_capabilities::get_model_capabilities(
                &request.model_id,
            );
        if capabilities.is_reasoning_model {
            reasoning_model_rules(&mut options, kind, preset, &capabilities);
        }
    }
    // Same forwarding for the xAI Chat protocol's `max_completion_tokens`.
    if *kind == ProviderKind::Xai
        && let Some(max_tokens) = request.config.as_ref().and_then(|config| config.max_tokens)
    {
        options
            .provider_options
            .get_or_insert_default()
            .entry("xai".into())
            .or_default()
            .insert("max_completion_tokens".into(), json!(max_tokens as u32));
    }
    // Anthropic can only continue stored thinking when this request asks for
    // thinking; with it off every stored thinking part is left out, exactly
    // as a part without continuation data for this protocol would be.
    if *kind == ProviderKind::Anthropic
        && !options
            .provider_options
            .as_ref()
            .and_then(|p| p.get("anthropic"))
            .and_then(|p| p.get("thinking"))
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "enabled" | "adaptive"))
    {
        let mut dropped = 0;
        for message in &mut options.prompt {
            if let LanguageModelMessage::Assistant { content, .. } = message {
                content.retain(|part| {
                    let keep = !matches!(part, AssistantPart::Reasoning(_));
                    dropped += usize::from(!keep);
                    keep
                });
            }
        }
        for _ in 0..dropped {
            warn_dropped(kind);
        }
    }
    let call_id = request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.extensions.get(crate::plugin::REQUEST_ID_EXTENSION))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let startup = request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.startup.as_ref())
        .is_some_and(|startup| startup.connection.is_some());
    let recording = crate::recording::capture(call_id, startup);
    Ok(PreparedRequest {
        options,
        tool_names,
        recording,
    })
}

/// The request rules main applied to a model the tag classifies as a
/// reasoning model, as far as the two Chat models cannot apply them
/// themselves: no sampling value the model rejects, and, on the compatible
/// model, the token key and the `developer` role.
fn reasoning_model_rules(
    options: &mut CallOptions,
    kind: &ProviderKind,
    preset: Option<&str>,
    capabilities: &aimux_providers::openai::language_model_capabilities::ModelCapabilities,
) {
    let namespace = if *kind == ProviderKind::OpenaiChat {
        "openai"
    } else {
        preset.unwrap_or("openai")
    };
    let effort = options
        .provider_options
        .as_ref()
        .and_then(|options| options.get(namespace))
        .and_then(|options| options.get("reasoningEffort"))
        .and_then(Value::as_str);
    // A reasoning model takes a sampling value only with reasoning turned off,
    // and only where the model takes a non-reasoning parameter at all. The
    // OpenAI Chat model would otherwise drop the value and warn, and a strict
    // stream fails the call on that warning.
    if !(effort == Some("none") && capabilities.supports_non_reasoning_parameters) {
        options.temperature = None;
        options.top_p = None;
    }
    if *kind == ProviderKind::OpenaiChat {
        return;
    }
    // A registry row that names its own token key keeps it: the preset builds
    // its model with that key. Anything else sends `max_completion_tokens`
    // through the model's own namespace.
    let row_key = preset
        .and_then(aimux_providers::preset::lookup)
        .and_then(|entry| entry.descriptor.max_tokens_key.as_deref());
    if row_key.is_none()
        && let Some(max_tokens) = options.max_output_tokens.take()
    {
        options
            .provider_options
            .get_or_insert_default()
            .entry(namespace.into())
            .or_default()
            .insert("max_completion_tokens".into(), json!(max_tokens));
    }
    // The compatible model writes every system message with the `system` role
    // and has no setting for another. It spreads a message's
    // `openaiCompatible` metadata onto the wire message, which is where the
    // role main sent travels.
    for message in &mut options.prompt {
        if let LanguageModelMessage::System {
            provider_options, ..
        } = message
        {
            *provider_options = Some(
                provider_namespace("openaiCompatible", json!({"role": "developer"}))
                    .expect("a message role is an object"),
            );
        }
    }
}

/// One user message, built as the user role's own part type.
fn user_message(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    content: &MessageContent,
) -> Result<LanguageModelMessage, HostError> {
    let content = match content {
        MessageContent::Text(text) => vec![UserPart::Text(TextPart {
            text: text.clone(),
            provider_options: None,
        })],
        MessageContent::Parts(parts) => {
            let mut content = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                let path = format!("{path}[{index}]");
                let mut provider_options = None;
                let include =
                    continuation(request, kind, &path, part, &mut provider_options, false)?;
                match part {
                    MessagePart::Text { text, .. } if include => {
                        content.push(UserPart::Text(TextPart {
                            text: text.clone(),
                            provider_options,
                        }));
                    }
                    MessagePart::Image { source } if include => {
                        content.push(image_part(request, &path, source)?);
                    }
                    MessagePart::Text { .. } | MessagePart::Image { .. } => {}
                    _ => {
                        return Err(error(
                            request,
                            &path,
                            "content cannot be represented for this message role",
                        ));
                    }
                }
            }
            content
        }
    };
    Ok(LanguageModelMessage::User {
        content,
        provider_options: None,
    })
}

/// One assistant message, built as the assistant role's own part type. The
/// tool names this message registers stay in force for the results after it.
fn assistant_message<'a>(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    content: &'a MessageContent,
    tool_names: &mut ToolNames,
    calls: &mut HashMap<&'a str, String>,
) -> Result<LanguageModelMessage, HostError> {
    let content = match content {
        MessageContent::Text(text) => vec![AssistantPart::Text(TextPart {
            text: text.clone(),
            provider_options: None,
        })],
        MessageContent::Parts(parts) => {
            let mut content = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                let path = format!("{path}[{index}]");
                let later_encrypted = *kind == ProviderKind::OpenaiResponses
                    && later_encrypted(&parts[index + 1..], part);
                let mut provider_options = None;
                let include = continuation(
                    request,
                    kind,
                    &path,
                    part,
                    &mut provider_options,
                    later_encrypted,
                )?;
                match part {
                    MessagePart::Text { text, .. } if include => {
                        content.push(AssistantPart::Text(TextPart {
                            text: text.clone(),
                            provider_options,
                        }));
                    }
                    MessagePart::Thinking { thinking, .. } if include => {
                        content.push(AssistantPart::Reasoning(ReasoningPart {
                            text: thinking.clone(),
                            provider_options,
                        }));
                    }
                    MessagePart::ToolUse {
                        id, name, input, ..
                    } if include => {
                        let name = tool_names
                            .register(name)
                            .map_err(|reason| error(request, &format!("{path}.name"), &reason))?;
                        calls.insert(id.as_str(), name.clone());
                        content.push(AssistantPart::ToolCall(ToolCallPart {
                            tool_call_id: id.clone(),
                            tool_name: name,
                            input: Value::Object(input.clone()),
                            provider_executed: None,
                            provider_options,
                        }));
                    }
                    MessagePart::Text { .. }
                    | MessagePart::Thinking { .. }
                    | MessagePart::ToolUse { .. } => {}
                    _ => {
                        return Err(error(
                            request,
                            &path,
                            "content cannot be represented for this message role",
                        ));
                    }
                }
            }
            content
        }
    };
    Ok(LanguageModelMessage::Assistant {
        content,
        provider_options: None,
    })
}

/// One tool message, built as the tool role's own part type. The result has
/// to name a tool call an earlier assistant message registered.
fn tool_message<'a>(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    content: &'a MessageContent,
    calls: &HashMap<&'a str, String>,
    following: &mut Vec<UserPart>,
) -> Result<LanguageModelMessage, HostError> {
    let content = match content {
        MessageContent::Text(_) => {
            return Err(error(request, path, "tool results require a tool call ID"));
        }
        MessageContent::Parts(parts) => {
            let mut content = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                let path = format!("{path}[{index}]");
                let MessagePart::ToolResult {
                    tool_use_id,
                    content: tool_content,
                    is_error,
                    ..
                } = part
                else {
                    return Err(error(
                        request,
                        &path,
                        "content cannot be represented for this message role",
                    ));
                };
                let name = calls.get(tool_use_id.as_str()).ok_or_else(|| {
                    error(
                        request,
                        &format!("{path}.tool_use_id"),
                        "no preceding tool call",
                    )
                })?;
                let output = tool_result(
                    request,
                    kind,
                    &path,
                    tool_use_id,
                    tool_content,
                    *is_error == Some(true),
                    following,
                )?;
                let mut provider_options = None;
                if continuation(request, kind, &path, part, &mut provider_options, false)? {
                    content.push(ToolPart::ToolResult(ToolResultPart {
                        tool_call_id: tool_use_id.clone(),
                        tool_name: name.clone(),
                        output,
                        provider_options,
                    }));
                }
            }
            content
        }
    };
    Ok(LanguageModelMessage::Tool {
        content,
        provider_options: None,
    })
}

/// An image part as the protocols' user messages carry it.
fn image_part(
    request: &ModelRequest,
    path: &str,
    source: &ImageSource,
) -> Result<UserPart, HostError> {
    let data = match source.source_type {
        ImageSourceType::File => {
            return Err(HostError::classified(
                HostErrorKind::InvalidRequest,
                "Image file was not materialized at the model boundary",
            ));
        }
        ImageSourceType::Base64 => {
            let data = source.data.as_ref().ok_or_else(|| {
                error(
                    request,
                    &format!("{path}.source.data"),
                    "base64 image has no data",
                )
            })?;
            FileData::Data {
                data: FileBytes::Base64(data.clone()),
            }
        }
        ImageSourceType::Url => {
            let url = source.url.as_ref().ok_or_else(|| {
                error(
                    request,
                    &format!("{path}.source.url"),
                    "URL image has no URL",
                )
            })?;
            FileData::Url {
                url: url.clone(),
                original_url: None,
            }
        }
    };
    Ok(UserPart::File(FilePart {
        data,
        media_type: source.media_type.clone(),
        filename: None,
        provider_options: None,
    }))
}

/// A tool result's output. Protocols whose tool message carries an image keep
/// it inside the result; the others mark its place in the text and put the
/// image in `following` (one user message after the step's tools).
fn tool_result(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    tool_use_id: &str,
    content: &ToolContent,
    is_error: bool,
    following: &mut Vec<UserPart>,
) -> Result<ToolResultOutput, HostError> {
    let blocks = match content {
        ToolContent::Text(text) => {
            return Ok(if is_error {
                ToolResultOutput::ErrorText {
                    value: text.clone(),
                    provider_options: None,
                }
            } else {
                ToolResultOutput::Text {
                    value: text.clone(),
                    provider_options: None,
                }
            });
        }
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
        return Ok(if is_error {
            ToolResultOutput::ErrorText {
                value: text,
                provider_options: None,
            }
        } else {
            ToolResultOutput::Text {
                value: text,
                provider_options: None,
            }
        });
    }
    let inside = matches!(kind, ProviderKind::Anthropic | ProviderKind::Bedrock);
    let mut result = Vec::with_capacity(blocks.len());
    let mut text = String::new();
    for block in blocks {
        let marker = match block {
            ContentBlock::Text { text: block } => {
                if inside {
                    result.push(ToolResultContent::Text(TextPart {
                        text: block.clone(),
                        provider_options: None,
                    }));
                } else {
                    text.push_str(block);
                }
                continue;
            }
            ContentBlock::Image { source } => match (inside, base64_data(source)) {
                (true, Some(data)) => {
                    result.push(inside_image(source, data));
                    continue;
                }
                _ => {
                    following.push(UserPart::Text(TextPart {
                        text: format!(
                            "{TOOL_IMAGE_LABEL} {tool_use_id}: the image after this text is that call's result."
                        ),
                        provider_options: None,
                    }));
                    following.push(image_part(request, path, source)?);
                    TOOL_IMAGE_MARKER
                }
            },
        };
        if inside {
            result.push(ToolResultContent::Text(TextPart {
                text: marker.to_owned(),
                provider_options: None,
            }));
        } else {
            text.push_str(marker);
        }
    }
    if inside {
        Ok(ToolResultOutput::Content {
            value: result,
            is_error: is_error.then_some(true),
        })
    } else if is_error {
        Ok(ToolResultOutput::ErrorText {
            value: text,
            provider_options: None,
        })
    } else {
        Ok(ToolResultOutput::Text {
            value: text,
            provider_options: None,
        })
    }
}

fn base64_data(source: &ImageSource) -> Option<&str> {
    if source.source_type != ImageSourceType::Base64 {
        return None;
    }
    source.data.as_deref()
}

/// An image inside a tool result, in the form both protocols' converters read.
fn inside_image(source: &ImageSource, data: &str) -> ToolResultContent {
    ToolResultContent::File(FilePart {
        data: FileData::Data {
            data: FileBytes::Base64(data.to_owned()),
        },
        media_type: source.media_type.clone(),
        filename: None,
        provider_options: None,
    })
}

fn error(request: &ModelRequest, path: &str, reason: &str) -> HostError {
    HostError::failed(format!(
        "{}/{}: {path}: {reason}",
        request.provider_id, request.model_id
    ))
}

/// The protocol a stored part's continuation data belongs to. The Chat
/// protocols store none, so every metadata value there is another protocol's.
fn metadata_protocol(metadata: &ContentMetadata) -> ProviderKind {
    match metadata {
        ContentMetadata::Bedrock { .. } | ContentMetadata::BedrockRedacted { .. } => {
            ProviderKind::Bedrock
        }
        ContentMetadata::Anthropic { .. } | ContentMetadata::AnthropicRedacted { .. } => {
            ProviderKind::Anthropic
        }
        ContentMetadata::Gemini { .. } => ProviderKind::Gemini,
        ContentMetadata::XaiResponses { .. } => ProviderKind::XaiResponses,
        ContentMetadata::OpenaiResponses { .. } => ProviderKind::OpenaiResponses,
    }
}

/// Attaches the native continuation fields a stored part carries, and says
/// whether the part belongs in the request. A thinking part without usable
/// continuation data for this protocol stays in the Timeline and in display,
/// but is not sent back: the endpoint cannot continue it, and an empty or
/// invented signature is rejected by a strict Anthropic implementation.
fn continuation(
    request: &ModelRequest,
    kind: &ProviderKind,
    path: &str,
    part: &MessagePart,
    provider_options: &mut Option<SharedProviderOptions>,
    later_encrypted: bool,
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
    // Continuation data reaches only the protocol that produced it; data from
    // another protocol is no data for this request.
    let metadata = metadata.filter(|metadata| metadata_protocol(metadata) == *kind);
    let Some(metadata) = metadata else {
        if thinking
            && matches!(
                kind,
                ProviderKind::Anthropic
                    | ProviderKind::OpenaiResponses
                    | ProviderKind::XaiResponses
                    | ProviderKind::Bedrock
            )
        {
            warn_dropped(kind);
            return Ok(false);
        }
        return Ok(true);
    };
    let (namespace, fields) = match (kind, metadata) {
        (ProviderKind::Bedrock, ContentMetadata::Bedrock { signature }) if thinking => {
            match signature {
                Some(signature) => ("bedrock", json!({"signature":signature})),
                None => ("bedrock", json!({})),
            }
        }
        (ProviderKind::Bedrock, ContentMetadata::BedrockRedacted { data }) if thinking => {
            // The canonical namespace key; the model also reads the older
            // `redactedData` spelling, which reaches the wire in another form.
            ("bedrock", json!({"redactedContent":data}))
        }
        (ProviderKind::Anthropic, ContentMetadata::Anthropic { signature }) if thinking => {
            if signature.is_empty() {
                warn_dropped(kind);
                return Ok(false);
            }
            ("anthropic", json!({"signature":signature}))
        }
        (ProviderKind::Anthropic, ContentMetadata::AnthropicRedacted { data }) if thinking => {
            if data.is_empty() {
                warn_dropped(kind);
                return Ok(false);
            }
            ("anthropic", json!({"redactedData":data}))
        }
        (ProviderKind::Gemini, ContentMetadata::Gemini { thought_signature }) => {
            ("google", json!({"thoughtSignature":thought_signature}))
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
            ("xai", Value::Object(options))
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
            if thinking
                && encrypted_content.as_ref().is_none_or(String::is_empty)
                && !later_encrypted
            {
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
            ("openai", Value::Object(options))
        }
        _ => {
            return Err(error(
                request,
                &format!("{path}.providerMetadata"),
                "continuation metadata does not fit this content kind",
            ));
        }
    };
    *provider_options =
        Some(provider_namespace(namespace, fields).expect("continuation fields are an object"));
    Ok(true)
}

/// Whether a later part of the same assistant message continues the same
/// OpenAI reasoning item with encrypted content. The Responses endpoint takes
/// the item's summary parts together, so an earlier summary-only part belongs
/// in the request even though it cannot continue the item by itself; a lone
/// unsigned part cannot, and is left out.
fn later_encrypted(rest: &[MessagePart], part: &MessagePart) -> bool {
    let item_id = match part {
        MessagePart::Thinking {
            provider_metadata:
                Some(ContentMetadata::OpenaiResponses {
                    item_id,
                    encrypted_content: None,
                    ..
                }),
            ..
        } => item_id,
        _ => return false,
    };
    rest.iter().any(|part| {
        matches!(
            part,
            MessagePart::Thinking {
                provider_metadata:
                    Some(ContentMetadata::OpenaiResponses {
                        item_id: later,
                        encrypted_content: Some(encrypted),
                        ..
                    }),
                ..
            } if later == item_id && !encrypted.is_empty()
        )
    })
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
        "thinking without usable continuation data was left out of the request"
    );
}
