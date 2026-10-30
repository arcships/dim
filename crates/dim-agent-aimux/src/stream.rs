mod content;

use std::collections::{HashMap, HashSet};

use aimux_core::{
    AbortSignal, AiMuxError,
    parse_tool_call::{RawToolCall, parse_json_input},
    result::StreamResult,
    stream_part::StreamPart,
    types::{FinishReasonUnified, Usage as NativeUsage, Warning},
};
use dim_agent_capabilities::host::{
    HostError, HostErrorKind, ModelChunk, ModelFailureFacts, ModelFailureOrigin, ModelProtocol,
    ModelStopReason, ModelStream,
};
use dim_agent_types::{ContentMetadata, MessagePart, ToolCall, Usage};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::request::ToolNames;

pub(crate) fn convert(
    result: StreamResult,
    tool_names: ToolNames,
    protocol: ModelProtocol,
    cancel: CancellationToken,
    abort: AbortSignal,
    generation: CancellationToken,
    lease: tokio_util::task::task_tracker::TaskTrackerToken,
) -> ModelStream {
    let StreamResult { mut stream, .. } = result;
    Box::pin(async_stream::stream! {
        let lease = lease;
        let message_id = uuid::Uuid::new_v4().to_string();
        let mut calls = HashMap::<String, String>::new();
        let preserve = !matches!(protocol, ModelProtocol::OpenaiChat | ModelProtocol::OpenaiCompatible);
        let mut parts = Vec::new();
        let mut blocks = HashMap::new();
        let mut tool_blocks = HashMap::new();
        let mut block_active = HashSet::new();
        let mut ended_inputs = HashSet::new();
        let mut completed_calls = HashSet::new();
        let mut thinking = HashMap::<String, String>::new();
        let terminal = loop {
            let next = tokio::select! {
                biased;
                _ = generation.cancelled() => {
                    break Some(Err(HostError::classified(HostErrorKind::OwnerClosed, "Aimux owner is closed")));
                }
                _ = cancel.cancelled() => {
                    break Some(Err(HostError::Cancelled("Model request cancelled".into())));
                }
                next = stream.next() => next,
            };
            let Some(next) = next else { break None };
            let part = match next {
                Ok(part) => part,
                Err(failure) => break Some(Err(error(failure, &cancel))),
            };
            match part {
                StreamPart::StreamStart { warnings } => {
                    let mut unsupported = None;
                    for warning in warnings {
                        match warning {
                            Warning::Unsupported { feature, .. } => {
                                unsupported = Some(HostError::classified(HostErrorKind::Unsupported, format!(
                                    "Unsupported model parameter: {feature}"
                                )));
                                break;
                            }
                            Warning::Compatibility { .. } => {
                                tracing::debug!(warning_kind = "compatibility", "aimux warning");
                            }
                            Warning::Deprecated { .. } => {
                                tracing::debug!(warning_kind = "deprecated", "aimux warning");
                            }
                            Warning::Other { .. } => {
                                tracing::debug!(warning_kind = "other", "aimux warning");
                            }
                        }
                    }
                    if let Some(failure) = unsupported {
                        break Some(Err(failure));
                    }
                    yield Ok(ModelChunk::MessageStart { message_id: message_id.clone() });
                }
                StreamPart::TextStart { id, provider_metadata } => {
                    if !block_active.insert((false, id.clone())) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Duplicate text start".into()), &cancel)));
                    }
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        blocks.insert((false, id), parts.len());
                        parts.push(MessagePart::Text { text: String::new(), start_time: None, end_time: None, provider_metadata: metadata });
                    }
                }
                StreamPart::ReasoningStart { id, provider_metadata } => {
                    if !block_active.insert((true, id.clone())) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Duplicate reasoning start".into()), &cancel)));
                    }
                    thinking.insert(id.clone(), String::new());
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        blocks.insert((true, id), parts.len());
                        parts.push(MessagePart::Thinking { thinking: String::new(), start_time: None, end_time: None, provider_metadata: metadata });
                    }
                }
                StreamPart::TextDelta { id, delta, provider_metadata } => {
                    if !block_active.contains(&(false, id.clone())) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Text delta without an active text block".into()), &cancel)));
                    }
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.get(&(false, id)).and_then(|index| parts.get_mut(*index)) else {
                            break Some(Err(error(AiMuxError::InvalidResponseData("Missing text block".into()), &cancel)));
                        };
                        if let MessagePart::Text { text, .. } = part { text.push_str(&delta); }
                        content::update(part, metadata);
                    }
                    if !delta.is_empty() { yield Ok(ModelChunk::Text(delta)); }
                }
                StreamPart::ReasoningDelta { id, delta, provider_metadata } => {
                    if !block_active.contains(&(true, id.clone())) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Reasoning delta without an active reasoning block".into()), &cancel)));
                    }
                    let Some(thought) = thinking.get_mut(&id) else {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Missing reasoning block".into()), &cancel)));
                    };
                    thought.push_str(&delta);
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.get(&(true, id)).and_then(|index| parts.get_mut(*index)) else {
                            break Some(Err(error(AiMuxError::InvalidResponseData("Missing reasoning content".into()), &cancel)));
                        };
                        if let MessagePart::Thinking { thinking, .. } = part { thinking.push_str(&delta); }
                        content::update(part, metadata);
                    }
                    if !delta.is_empty() { yield Ok(ModelChunk::Thinking(delta)); }
                }
                end @ (StreamPart::TextEnd { .. } | StreamPart::ReasoningEnd { .. }) => {
                    let reasoning = matches!(&end, StreamPart::ReasoningEnd { .. });
                    let (id, provider_metadata) = match end { StreamPart::TextEnd { id, provider_metadata } | StreamPart::ReasoningEnd { id, provider_metadata } => (id, provider_metadata), _ => unreachable!() };
                    if !block_active.remove(&(reasoning, id.clone())) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Block end without a matching active block".into()), &cancel)));
                    }
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.remove(&(reasoning, id.clone())).and_then(|index| parts.get_mut(index)) else {
                            break Some(Err(error(AiMuxError::InvalidResponseData("Missing ended block".into()), &cancel)));
                        };
                        content::update(part, metadata);
                    }
                    if reasoning {
                        yield Ok(ModelChunk::ThinkingEnd { thinking: thinking.remove(&id).unwrap_or_default() });
                    } else {
                        yield Ok(ModelChunk::TextEnd);
                    }
                }
                StreamPart::ToolInputStart { id, tool_name, provider_executed, provider_metadata, .. } => {
                    if calls.contains_key(&id) || ended_inputs.contains(&id) || completed_calls.contains(&id) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Duplicate tool input start".into()), &cancel)));
                    }
                    if provider_executed == Some(true) {
                        break Some(Err(HostError::failed("Provider-executed tools are not supported")));
                    }
                    let name = tool_names.original(&tool_name).to_owned();
                    calls.insert(id.clone(), name.clone());
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        tool_blocks.insert(id.clone(), parts.len());
                        parts.push(MessagePart::ToolUse { id: id.clone(), name: name.clone(), input: Default::default(), start_time: None, provider_metadata: metadata });
                    }
                    yield Ok(ModelChunk::ToolStart { id, name });
                }
                StreamPart::ToolInputDelta { id, delta, .. } => {
                    let Some(name) = calls.get(&id).filter(|_| !ended_inputs.contains(&id)).cloned() else {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Tool delta without an active tool input".into()), &cancel)));
                    };
                    yield Ok(ModelChunk::ToolArguments { id, name, delta });
                }
                StreamPart::ToolInputEnd { id, .. } => {
                    if completed_calls.contains(&id) || !ended_inputs.insert(id) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Duplicate or late tool input end".into()), &cancel)));
                    }
                }
                StreamPart::ToolCall {
                    tool_call_id, tool_name, input, provider_executed, dynamic,
                    thought_signature, invalid, error: failure, provider_metadata,
                } => {
                    let name = tool_names.original(&tool_name).to_owned();
                    if completed_calls.contains(&tool_call_id) || calls.get(&tool_call_id).is_some_and(|started| started != &name) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Duplicate or mismatched tool call".into()), &cancel)));
                    }
                    if let Some(failure) = failure {
                        break Some(Err(error(failure, &cancel)));
                    }
                    if invalid == Some(true) {
                        break Some(Err(HostError::failed("aimux returned an invalid tool call")));
                    }
                    if provider_executed == Some(true) {
                        break Some(Err(HostError::failed("Provider-executed tools are not supported")));
                    }
                    if thought_signature.is_some() && protocol != ModelProtocol::Gemini {
                        break Some(Err(HostError::failed("Tool thought signatures are not supported")));
                    }
                    let Value::String(input) = input else {
                        break Some(Err(HostError::failed("aimux do_stream tool input must be serialized argument text")));
                    };
                    let raw = RawToolCall {
                        tool_call_id, tool_name, input, provider_executed, dynamic,
                        thought_signature, provider_metadata,
                    };
                    let input = match parse_json_input(&raw) {
                        Ok(Value::Object(input)) => input,
                        Ok(_) => break Some(Err(HostError::failed("Tool arguments must be a JSON object"))),
                        Err(failure) => break Some(Err(error(failure, &cancel))),
                    };
                    let name = tool_names.original(&raw.tool_name).to_owned();
                    let started = calls.remove(&raw.tool_call_id).is_some();
                    let id = raw.tool_call_id.clone();
                    ended_inputs.remove(&id);
                    completed_calls.insert(id.clone());
                    if preserve {
                        let metadata = match raw.thought_signature.as_ref() {
                            Some(signature) => Some(ContentMetadata::Gemini { thought_signature: signature.clone() }),
                            None => match content::metadata(protocol, raw.provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) },
                        };
                        if let Some(index) = tool_blocks.remove(&id) {
                            let part = &mut parts[index];
                            if let MessagePart::ToolUse { input: arguments, .. } = part { *arguments = input.clone(); }
                            content::update(part, metadata);
                        } else {
                            parts.push(MessagePart::ToolUse { id: id.clone(), name: name.clone(), input: input.clone(), start_time: None, provider_metadata: metadata });
                        }
                    }
                    drop(raw);
                    if !started {
                        yield Ok(ModelChunk::ToolStart { id: id.clone(), name: name.clone() });
                    }
                    yield Ok(ModelChunk::ToolUse(ToolCall { id, name, input }));
                }
                StreamPart::Finish { finish_reason, usage, .. } => {
                    let reason = match finish_reason.unified {
                        FinishReasonUnified::Stop => ModelStopReason::EndTurn,
                        FinishReasonUnified::ToolCalls => ModelStopReason::ToolUse,
                        FinishReasonUnified::Length => ModelStopReason::MaxTokens,
                        FinishReasonUnified::ContentFilter => {
                            break Some(Err(HostError::failed("Model output was blocked by a content filter")));
                        }
                        FinishReasonUnified::Error => {
                            break Some(Err(HostError::failed("Model generation failed")));
                        }
                        FinishReasonUnified::Other => {
                            break Some(Err(HostError::failed("Model returned an unsupported finish reason")));
                        }
                    };
                    if reason != ModelStopReason::MaxTokens && (!block_active.is_empty() || !calls.is_empty() || !ended_inputs.is_empty()) {
                        break Some(Err(error(AiMuxError::InvalidResponseData("Model finished with incomplete blocks or tool calls".into()), &cancel)));
                    }
                    break Some(Ok((usage_chunk(usage), reason)));
                }
                StreamPart::Error { error: failure } => break Some(Err(error(failure, &cancel))),
                StreamPart::ToolResult { .. } => {
                    break Some(Err(HostError::failed("Provider-executed tool results are not supported")));
                }
                StreamPart::File { .. } => {
                    break Some(Err(HostError::failed("Generated files are not supported")));
                }
                StreamPart::Source { .. } => {
                    break Some(Err(HostError::failed("Model source results are not supported")));
                }
                StreamPart::ResponseMetadata { .. }
                | StreamPart::Raw { .. } => {}
            }
        };

        if matches!(terminal, Some(Err(_))) {
            abort.abort();
        }
        drop(stream);
        drop(calls);
        drop(thinking);
        drop(tool_names);
        drop(abort);
        drop(lease);

        match terminal {
            Some(Ok((usage, reason))) => {
                if preserve && reason != ModelStopReason::MaxTokens { yield Ok(ModelChunk::Content(parts)); }
                if let Some(usage) = usage {
                    yield Ok(ModelChunk::Usage(usage));
                }
                yield Ok(ModelChunk::MessageStop { message_id, reason });
            }
            Some(Err(failure)) => yield Err(failure),
            None => {}
        }
    })
}

fn usage_chunk(usage: NativeUsage) -> Option<Usage> {
    let prompt_tokens = usage.input_tokens.total?;
    let completion_tokens = usage.output_tokens.total?;
    Some(Usage {
        prompt_tokens: f64::from(prompt_tokens),
        completion_tokens: f64::from(completion_tokens),
        total_tokens: (u64::from(prompt_tokens) + u64::from(completion_tokens)) as f64,
        cache_read_tokens: usage.input_tokens.cache_read.map(f64::from),
        cache_write_tokens: usage.input_tokens.cache_write.map(f64::from),
        cost: None,
        currency: None,
    })
}

pub(crate) fn error(failure: AiMuxError, cancel: &CancellationToken) -> HostError {
    if matches!(failure, AiMuxError::Aborted(_)) && cancel.is_cancelled() {
        return HostError::Cancelled("Model request cancelled".into());
    }
    let mut model = None;
    let classification = match &failure {
        AiMuxError::ApiCall(detail) => {
            let view = detail.data.as_ref().map(|data| {
                data.pointer("/response/error")
                    .or_else(|| data.get("error"))
                    .unwrap_or(data)
            });
            let field = |name: &str| {
                view.and_then(|value| value.get(name))
                    .and_then(Value::as_str)
            };
            let normalize = |value: &str| {
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_ascii_lowercase())
            };
            let code = field("code").and_then(normalize);
            let error_type = field("type").and_then(normalize);
            let provider_code = detail.provider_code.as_deref().and_then(normalize);
            let origin = match detail.origin {
                aimux_core::ApiCallErrorOrigin::Transport => Some(ModelFailureOrigin::Transport),
                aimux_core::ApiCallErrorOrigin::HttpResponse => {
                    Some(ModelFailureOrigin::HttpResponse)
                }
                aimux_core::ApiCallErrorOrigin::StreamEvent => {
                    Some(ModelFailureOrigin::StreamEvent)
                }
                _ => None,
            };
            if let Some(origin) = origin {
                let codes = [
                    code.as_deref(),
                    error_type.as_deref(),
                    provider_code.as_deref(),
                ];
                let permanent = codes.iter().flatten().any(|code| {
                    matches!(
                        *code,
                        "insufficient_quota"
                            | "quota_exceeded"
                            | "billing_error"
                            | "credit_exhausted"
                            | "invalid_api_key"
                            | "authentication_error"
                            | "invalid_request_error"
                            | "context_length_exceeded"
                            | "context_too_long"
                            | "max_tokens_exceeded"
                            | "content_policy_violation"
                            | "safety_block"
                            | "moderation_block"
                    )
                });
                let transient_code = codes.iter().flatten().any(|code| {
                    matches!(
                        *code,
                        "rate_limit_exceeded"
                            | "rate_limit_error"
                            | "request_timeout"
                            | "timeout"
                            | "server_error"
                            | "api_error"
                            | "service_unavailable"
                            | "engine_overloaded"
                            | "model_overloaded"
                            | "server_busy"
                            | "overloaded_error"
                    )
                });
                let transient = !permanent
                    && !matches!(detail.status_code, Some(409 | 501 | 505))
                    && (origin == ModelFailureOrigin::Transport
                        || transient_code
                        || matches!(
                            detail.status_code,
                            Some(408 | 429 | 500 | 502..=504 | 520..=527 | 529)
                        ));
                let request_id = detail.response_headers.as_ref().and_then(|headers| {
                    ["x-request-id", "request-id", "x-amzn-requestid"]
                        .iter()
                        .find_map(|name| headers.get(*name))
                        .cloned()
                });
                model = Some(Box::new(ModelFailureFacts {
                    origin,
                    error_type,
                    message: field("message")
                        .filter(|value| !value.trim().is_empty())
                        .map(|_| detail.message.clone()),
                    request_id,
                    transient,
                    retry_after: failure
                        .retry_after_hint()
                        .map(|ms| std::time::Duration::from_millis(ms as u64)),
                }));
            }
            HostErrorKind::Http {
                status: detail.status_code,
                code,
            }
        }
        AiMuxError::InvalidArgument(_) | AiMuxError::InvalidPrompt(_) => {
            HostErrorKind::InvalidRequest
        }
        AiMuxError::UnsupportedFunctionality(_) => HostErrorKind::Unsupported,
        AiMuxError::Other(_) | AiMuxError::Timeout(_) => HostErrorKind::Transport,
        _ => HostErrorKind::Other,
    };
    let (kind, message) = error_message(failure);
    tracing::debug!(error_kind = kind, "aimux request failed");
    HostError::Failed {
        kind: classification,
        message,
        budget: None,
        model,
        presentation: None,
    }
}

fn error_message(failure: AiMuxError) -> (&'static str, String) {
    match failure {
        AiMuxError::ApiCall(detail) => {
            tracing::debug!(
                status = detail.status_code,
                provider_code = detail.provider_code.as_deref(),
                "aimux API call failed"
            );
            let mut message = format!("ApiCall: {detail}");
            if let Some(code) = detail.provider_code {
                message.push_str(&format!(" (provider code: {code})"));
            }
            ("api_call", message)
        }
        AiMuxError::InvalidToolInput { tool_name, .. } => (
            "invalid_tool_input",
            format!("InvalidToolInput: invalid arguments for tool '{tool_name}'"),
        ),
        AiMuxError::ToolCallRepair { cause, .. } => (
            "tool_call_repair",
            format!("ToolCallRepair: {}", error_message(*cause).1),
        ),
        AiMuxError::Retry(mut retry) => (
            "retry",
            match retry.errors.pop() {
                Some(failure) => format!("Retry: {}", error_message(failure).1),
                None => "Retry: failed without recorded errors".into(),
            },
        ),
        failure => {
            let kind = match &failure {
                AiMuxError::JsonParse(_) => "json_parse",
                AiMuxError::InvalidResponseData(_) => "invalid_response_data",
                AiMuxError::NoSuchTool { .. } => "no_such_tool",
                AiMuxError::InvalidArgument(_) => "invalid_argument",
                AiMuxError::InvalidPrompt(_) => "invalid_prompt",
                AiMuxError::TokenExpired(_) => "token_expired",
                AiMuxError::UnsupportedFunctionality(_) => "unsupported_functionality",
                AiMuxError::NoSuchModel { .. } => "no_such_model",
                AiMuxError::NoSuchProvider { .. } => "no_such_provider",
                AiMuxError::Timeout(_) => "timeout",
                AiMuxError::Aborted(_) => "aborted",
                AiMuxError::Other(_) => "other",
                _ => unreachable!(),
            };
            (kind, format!("{kind}: {failure}"))
        }
    }
}

#[cfg(test)]
mod tests;
