mod content;

use std::collections::{HashMap, HashSet};

use aimux_core::{
    AbortSignal, AiMuxError, ApiCallErrorOrigin, ErrorCause,
    parse_tool_call::parse_json_input,
    result::StreamResult,
    shared::SharedProviderMetadata,
    stream_part::StreamPart,
    tool::RawToolCall,
    types::{FinishReasonUnified, Usage as NativeUsage, Warning},
};
use dim_agent_capabilities::host::{
    HostError, HostErrorKind, ModelChunk, ModelFailureFacts, ModelProtocol, ModelStopReason,
    ModelStream,
};
use dim_agent_types::{
    FAILURE_CAUSE_CHARS, FAILURE_CAUSE_FRAMES, FAILURE_REASON_CHARS, MessagePart, ModelFailureKind,
    ToolCall, Usage, one_line,
};
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
                    block_active.insert((false, id.clone()));
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        blocks.insert((false, id), parts.len());
                        parts.push(MessagePart::Text { text: String::new(), start_time: None, end_time: None, provider_metadata: metadata });
                    }
                }
                StreamPart::ReasoningStart { id, provider_metadata } => {
                    block_active.insert((true, id.clone()));
                    thinking.insert(id.clone(), String::new());
                    if preserve {
                        let metadata = match reasoning_metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        blocks.insert((true, id), parts.len());
                        parts.push(MessagePart::Thinking { thinking: String::new(), start_time: None, end_time: None, provider_metadata: metadata });
                    }
                }
                StreamPart::TextDelta { id, delta, provider_metadata } => {
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.get(&(false, id)).and_then(|index| parts.get_mut(*index)) else {
                            break Some(Err(error(AiMuxError::invalid_response_data("Missing text block"), &cancel)));
                        };
                        if let MessagePart::Text { text, .. } = part { text.push_str(&delta); }
                        content::update(part, metadata);
                    }
                    if !delta.is_empty() { yield Ok(ModelChunk::Text(delta)); }
                }
                StreamPart::ReasoningDelta { id, delta, provider_metadata } => {
                    let Some(thought) = thinking.get_mut(&id) else {
                        break Some(Err(error(AiMuxError::invalid_response_data("Missing reasoning block"), &cancel)));
                    };
                    thought.push_str(&delta);
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.get(&(true, id)).and_then(|index| parts.get_mut(*index)) else {
                            break Some(Err(error(AiMuxError::invalid_response_data("Missing reasoning content"), &cancel)));
                        };
                        if let MessagePart::Thinking { thinking, .. } = part { thinking.push_str(&delta); }
                        content::update(part, metadata);
                    }
                    if !delta.is_empty() { yield Ok(ModelChunk::Thinking(delta)); }
                }
                end @ (StreamPart::TextEnd { .. } | StreamPart::ReasoningEnd { .. }) => {
                    let reasoning = matches!(&end, StreamPart::ReasoningEnd { .. });
                    let (id, provider_metadata) = match end { StreamPart::TextEnd { id, provider_metadata } | StreamPart::ReasoningEnd { id, provider_metadata } => (id, provider_metadata), _ => unreachable!() };
                    block_active.remove(&(reasoning, id.clone()));
                    if preserve {
                        let metadata = match content::metadata(protocol, provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
                        let Some(part) = blocks.remove(&(reasoning, id.clone())).and_then(|index| parts.get_mut(index)) else {
                            break Some(Err(error(AiMuxError::invalid_response_data("Missing ended block"), &cancel)));
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
                    let Some(name) = calls.get(&id).cloned() else {
                        continue;
                    };
                    yield Ok(ModelChunk::ToolArguments { id, name, delta });
                }
                StreamPart::ToolInputEnd { id, .. } => {
                    ended_inputs.insert(id);
                }
                StreamPart::ToolCall(raw) => {
                    let RawToolCall { tool_call_id, tool_name, input, provider_executed, dynamic, provider_metadata } = raw;
                    let name = tool_names.original(&tool_name).to_owned();
                    if completed_calls.contains(&tool_call_id) || calls.get(&tool_call_id).is_some_and(|started| started != &name) {
                        break Some(Err(error(AiMuxError::invalid_response_data("Duplicate or mismatched tool call"), &cancel)));
                    }
                    if provider_executed == Some(true) {
                        break Some(Err(HostError::failed("Provider-executed tools are not supported")));
                    }
                    let raw = RawToolCall {
                        tool_call_id, tool_name, input, provider_executed, dynamic,
                        provider_metadata,
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
                        let metadata = match content::metadata(protocol, raw.provider_metadata.as_ref()) { Ok(metadata) => metadata, Err(error) => break Some(Err(error)) };
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
                        break Some(Err(error(AiMuxError::invalid_response_data("Model finished with incomplete blocks or tool calls"), &cancel)));
                    }
                    break Some(Ok((usage_chunk(usage), reason)));
                }
                StreamPart::Error { error: failure } => break Some(Err(error(failure, &cancel))),
                StreamPart::ToolResult(_) | StreamPart::ToolApprovalRequest(_) => {
                    break Some(Err(HostError::failed("Provider-executed tool results are not supported")));
                }
                StreamPart::File(_) | StreamPart::ReasoningFile(_) => {
                    break Some(Err(HostError::failed("Generated files are not supported")));
                }
                StreamPart::Source(_) => {
                    break Some(Err(HostError::failed("Model source results are not supported")));
                }
                StreamPart::ResponseMetadata(_)
                | StreamPart::Raw { .. }
                | StreamPart::Custom { .. } => {}
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

/// The metadata of a reasoning block. A Bedrock reasoning block without a
/// signature or redacted payload is still a Bedrock block, so it is stored as
/// an unsigned one; the converter leaves such a block out of a replayed
/// request, and the metadata only keeps its origin readable.
fn reasoning_metadata(
    protocol: ModelProtocol,
    value: Option<&SharedProviderMetadata>,
) -> Result<Option<dim_agent_types::ContentMetadata>, HostError> {
    let metadata = content::metadata(protocol, value)?;
    if metadata.is_none() && protocol == ModelProtocol::Bedrock {
        return Ok(Some(dim_agent_types::ContentMetadata::Bedrock {
            signature: None,
        }));
    }
    Ok(metadata)
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
        reasoning_tokens: None,
        cost: None,
        currency: None,
    })
}

pub(crate) fn error(failure: AiMuxError, cancel: &CancellationToken) -> HostError {
    if matches!(failure, AiMuxError::Aborted(_)) && cancel.is_cancelled() {
        return HostError::Cancelled("Model request cancelled".into());
    }
    // Classify and describe the same inner failure: a repair callback's
    // failure, a retry wrapper's last attempt and anything wrapped in `Other`
    // caused by another aimux error are unwrapped first.
    let failure = unwrap_failure(failure);
    let (classification, facts) = classify(&failure);
    tracing::debug!(
        failure_kind = ?facts.kind,
        status = facts.status,
        provider_code = facts.provider_code.as_deref(),
        "aimux request failed"
    );
    let message = facts.kind.line(facts.reason.as_deref());
    HostError::Failed {
        kind: classification,
        message,
        budget: None,
        model: Some(Box::new(facts)),
        presentation: None,
    }
}

fn unwrap_failure(failure: AiMuxError) -> AiMuxError {
    match failure {
        AiMuxError::Other {
            cause: Some(ErrorCause::AiMux(inner)),
            ..
        } => unwrap_failure(*inner),
        AiMuxError::ToolCallRepair { cause, .. } => unwrap_failure(*cause),
        AiMuxError::Retry(mut retry) => match retry.errors.pop() {
            Some(error) => unwrap_failure(error),
            None => AiMuxError::Retry(retry),
        },
        failure => failure,
    }
}

/// The internal host kind and the durable facts of one provider failure.
fn classify(failure: &AiMuxError) -> (HostErrorKind, ModelFailureFacts) {
    match failure {
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
            let provider_code = detail
                .provider_code
                .as_deref()
                .and_then(normalize)
                .and_then(|code| one_line(&code, FAILURE_CAUSE_CHARS));
            let origin = detail.origin;
            let codes = [
                code.as_deref(),
                error_type.as_deref(),
                provider_code.as_deref(),
            ];
            let kind = classify_kind(origin, detail.status_code, &codes);
            let chain = cause_chain(detail);
            let reason = if origin == ApiCallErrorOrigin::Transport {
                chain
                    .innermost
                    .clone()
                    .or_else(|| one_line(&detail.message, FAILURE_REASON_CHARS))
            } else {
                one_line(&detail.message, FAILURE_REASON_CHARS).or_else(|| chain.innermost.clone())
            };
            let request_id = detail.response_headers.as_ref().and_then(|headers| {
                ["x-request-id", "request-id", "x-amzn-requestid"]
                    .iter()
                    .find_map(|name| headers.get(*name))
                    .and_then(|id| one_line(id, FAILURE_CAUSE_CHARS))
            });
            let facts = ModelFailureFacts {
                kind,
                reason,
                status: detail.status_code,
                provider_code,
                request_id,
                cause: chain.frames,
                retry_after: failure
                    .retry_after_hint()
                    .map(|ms| std::time::Duration::from_millis(ms as u64)),
                attempts: 1,
                waited: std::time::Duration::ZERO,
                retry_after_stop: None,
                retry_forbidden: false,
            };
            (
                HostErrorKind::Http {
                    status: detail.status_code,
                    code,
                },
                facts,
            )
        }
        failure => {
            let (kind, classification) = match failure {
                AiMuxError::InvalidArgument { .. } | AiMuxError::InvalidPrompt { .. } => (
                    ModelFailureKind::InvalidRequest,
                    HostErrorKind::InvalidRequest,
                ),
                AiMuxError::UnsupportedFunctionality(_) => {
                    (ModelFailureKind::InvalidRequest, HostErrorKind::Unsupported)
                }
                AiMuxError::InvalidResponseData { .. }
                | AiMuxError::JsonParse { .. }
                | AiMuxError::InvalidToolInput { .. }
                | AiMuxError::NoSuchTool { .. }
                | AiMuxError::TypeValidation { .. } => {
                    (ModelFailureKind::MalformedResponse, HostErrorKind::Other)
                }
                AiMuxError::NoSuchModel { .. } | AiMuxError::NoSuchProvider { .. } => {
                    (ModelFailureKind::NotFound, HostErrorKind::Other)
                }
                AiMuxError::TokenExpired(_) => {
                    (ModelFailureKind::AuthRejected, HostErrorKind::Other)
                }
                AiMuxError::Timeout(_) => (ModelFailureKind::Unknown, HostErrorKind::Transport),
                AiMuxError::Other { .. } => (ModelFailureKind::Unknown, HostErrorKind::Transport),
                _ => (ModelFailureKind::Unknown, HostErrorKind::Other),
            };
            let facts = ModelFailureFacts {
                kind,
                reason: reason_of(failure),
                status: None,
                provider_code: None,
                request_id: None,
                cause: Vec::new(),
                retry_after: None,
                attempts: 1,
                waited: std::time::Duration::ZERO,
                retry_after_stop: None,
                retry_forbidden: false,
            };
            (classification, facts)
        }
    }
}

/// The reason of a failure with no provider boundary. A JSON parse error
/// carries the serde diagnosis as its cause; its message embeds the response
/// text, so the cause is the only safe source and a missing cause means no
/// reason.
fn reason_of(failure: &AiMuxError) -> Option<String> {
    match failure {
        AiMuxError::JsonParse { cause, .. } => cause.as_ref().and_then(source_diagnosis),
        AiMuxError::InvalidToolInput { tool_name, .. } => one_line(
            &format!("invalid arguments for tool '{tool_name}'"),
            FAILURE_REASON_CHARS,
        ),
        AiMuxError::NoSuchTool { tool_name, .. } => one_line(
            &format!("the model called an unknown tool '{tool_name}'"),
            FAILURE_REASON_CHARS,
        ),
        // Its message embeds the validated value.
        AiMuxError::TypeValidation { .. } => None,
        // The inner text only; `Display` adds the variant prefix.
        AiMuxError::Timeout(message) => one_line(message, FAILURE_REASON_CHARS),
        AiMuxError::Other {
            cause: Some(ErrorCause::AiMux(error)),
            ..
        }
        | AiMuxError::ToolCallRepair { cause: error, .. } => reason_of(error),
        AiMuxError::Retry(retry) => retry.errors.last().and_then(reason_of),
        _ => one_line(&failure.to_string(), FAILURE_REASON_CHARS),
    }
}

fn source_diagnosis(cause: &ErrorCause) -> Option<String> {
    match cause {
        ErrorCause::Source(source) => one_line(&source.message, FAILURE_REASON_CHARS),
        ErrorCause::AiMux(error) => reason_of(error),
    }
}

/// The user-facing kind, from the origin, status and normalized codes only.
fn classify_kind(
    origin: ApiCallErrorOrigin,
    status: Option<u16>,
    codes: &[Option<&str>],
) -> ModelFailureKind {
    if let Some(kind) = codes.iter().flatten().find_map(|code| permanent_kind(code)) {
        return kind;
    }
    if origin != ApiCallErrorOrigin::StreamEvent && matches!(status, Some(409 | 501 | 505)) {
        // These statuses were never retried, whatever code came with them.
        return ModelFailureKind::InvalidRequest;
    }
    let transient = codes.iter().flatten().find_map(|code| transient_kind(code));
    if origin == ApiCallErrorOrigin::StreamEvent {
        // A stream-event failure carries the response's status, not the
        // event's; only the event's code may decide.
        return transient.unwrap_or(ModelFailureKind::Unknown);
    }
    if origin == ApiCallErrorOrigin::Transport {
        return ModelFailureKind::Network;
    }
    if let Some(kind) = transient {
        return kind;
    }
    status_kind(status)
}

fn permanent_kind(code: &str) -> Option<ModelFailureKind> {
    Some(match code {
        "insufficient_quota" | "quota_exceeded" | "billing_error" | "credit_exhausted" => {
            ModelFailureKind::QuotaExhausted
        }
        "invalid_api_key" | "authentication_error" => ModelFailureKind::AuthRejected,
        "invalid_request_error" | "max_tokens_exceeded" => ModelFailureKind::InvalidRequest,
        "context_length_exceeded" | "context_too_long" => ModelFailureKind::ContextTooLong,
        "content_policy_violation" | "safety_block" | "moderation_block" => {
            ModelFailureKind::ContentFiltered
        }
        _ => return None,
    })
}

fn transient_kind(code: &str) -> Option<ModelFailureKind> {
    Some(match code {
        "rate_limit_exceeded" | "rate_limit_error" => ModelFailureKind::RateLimited,
        "request_timeout"
        | "timeout"
        | "server_error"
        | "api_error"
        | "service_unavailable"
        | "engine_overloaded"
        | "model_overloaded"
        | "server_busy"
        | "overloaded_error" => ModelFailureKind::ServerError,
        _ => return None,
    })
}

fn status_kind(status: Option<u16>) -> ModelFailureKind {
    match status {
        Some(401 | 403) => ModelFailureKind::AuthRejected,
        Some(402) => ModelFailureKind::QuotaExhausted,
        Some(404) => ModelFailureKind::NotFound,
        Some(408) => ModelFailureKind::ServerError,
        Some(413) => ModelFailureKind::ContextTooLong,
        Some(429) => ModelFailureKind::RateLimited,
        Some(400 | 409 | 422 | 501 | 505) => ModelFailureKind::InvalidRequest,
        Some(500..=599) => ModelFailureKind::ServerError,
        Some(400..=499) => ModelFailureKind::InvalidRequest,
        _ => ModelFailureKind::Unknown,
    }
}

/// The cause chain, outermost first: the failure's own message, then the
/// error it was built from. Frames are bounded; `innermost` is the last
/// non-empty frame of the whole chain, whether or not the frame list kept it.
struct CauseChain {
    frames: Vec<String>,
    innermost: Option<String>,
}

fn cause_chain(detail: &aimux_core::ApiCallError) -> CauseChain {
    let mut chain = CauseChain {
        frames: Vec::new(),
        innermost: None,
    };
    chain.push(&detail.message);
    if let Some(cause) = &detail.cause {
        chain.push_cause(cause);
    }
    chain
}

impl CauseChain {
    fn push(&mut self, text: &str) {
        if let Some(line) = one_line(text, FAILURE_CAUSE_CHARS) {
            self.innermost = Some(line.clone());
            if self.frames.len() < FAILURE_CAUSE_FRAMES {
                self.frames.push(line);
            }
        }
    }

    fn push_cause(&mut self, cause: &ErrorCause) {
        match cause {
            ErrorCause::Source(source) => self.push_source(source),
            ErrorCause::AiMux(error) => self.push(&error.to_string()),
        }
    }

    fn push_source(&mut self, source: &aimux_core::SourceError) {
        self.push(&source.message);
        if let Some(cause) = &source.cause {
            self.push_source(cause);
        }
    }
}

#[cfg(test)]
mod tests;
