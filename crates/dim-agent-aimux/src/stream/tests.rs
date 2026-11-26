use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use aimux_core::result::Source;
use aimux_core::types::{FinishReason, InputTokenUsage, OutputTokenUsage};
use futures_util::{FutureExt, Stream};
use serde_json::json;

use super::*;

struct NativeStream {
    parts: VecDeque<Result<StreamPart, AiMuxError>>,
    dropped: Arc<AtomicBool>,
    end_when_empty: bool,
}

impl Stream for NativeStream {
    type Item = Result<StreamPart, AiMuxError>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.parts.pop_front() {
            Some(part) => Poll::Ready(Some(part)),
            None if self.end_when_empty => Poll::Ready(None),
            None => Poll::Pending,
        }
    }
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn fixture(
    parts: Vec<Result<StreamPart, AiMuxError>>,
) -> (ModelStream, CancellationToken, AbortSignal, Arc<AtomicBool>) {
    fixture_with(parts, false)
}

/// A fixture whose stream ends instead of stalling once its parts are read.
fn fixture_ending(
    parts: Vec<Result<StreamPart, AiMuxError>>,
) -> (ModelStream, CancellationToken, AbortSignal, Arc<AtomicBool>) {
    fixture_with(parts, true)
}

fn fixture_with(
    parts: Vec<Result<StreamPart, AiMuxError>>,
    end_when_empty: bool,
) -> (ModelStream, CancellationToken, AbortSignal, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    let result = StreamResult {
        stream: Box::pin(NativeStream {
            parts: parts.into(),
            dropped: dropped.clone(),
            end_when_empty,
        }),
        request: None,
        response: None,
    };
    let cancel = CancellationToken::new();
    let abort = AbortSignal::new();
    let stream = convert(
        result,
        ToolNames::default(),
        ModelProtocol::OpenaiChat,
        cancel.clone(),
        abort.clone(),
        CancellationToken::new(),
        tokio_util::task::TaskTracker::new().token(),
    );
    (stream, cancel, abort, dropped)
}

/// The next chunk; a fixture stream that stalls instead of ending fails the
/// case rather than hanging the suite.
async fn next_or_stall(
    stream: &mut ModelStream,
    case: &str,
) -> Option<Result<ModelChunk, HostError>> {
    tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .unwrap_or_else(|_| panic!("{case}: the stream stalled instead of ending"))
}

/// Every chunk of `stream`; a fixture stream that stalls instead of ending
/// fails the case rather than hanging the suite.
async fn collect_or_stall(stream: ModelStream, case: &str) -> Vec<Result<ModelChunk, HostError>> {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.collect::<Vec<_>>(),
    )
    .await
    .unwrap_or_else(|_| panic!("{case}: the stream stalled instead of ending"))
}

/// A fixture that delivers only the parts the test feeds it and stalls while
/// its queue is empty, so a test can let virtual time pass inside one wait.
fn feedable() -> (ModelStream, Feed) {
    let parts = Arc::new(Mutex::new(VecDeque::new()));
    let result = StreamResult {
        stream: Box::pin(FeedStream {
            parts: parts.clone(),
        }),
        request: None,
        response: None,
    };
    let stream = convert(
        result,
        ToolNames::default(),
        ModelProtocol::OpenaiChat,
        CancellationToken::new(),
        AbortSignal::new(),
        CancellationToken::new(),
        tokio_util::task::TaskTracker::new().token(),
    );
    (stream, Feed(parts))
}

struct Feed(Arc<Mutex<VecDeque<Result<StreamPart, AiMuxError>>>>);

impl Feed {
    fn push(&self, parts: Vec<Result<StreamPart, AiMuxError>>) {
        self.0.lock().unwrap().extend(parts);
    }
}

struct FeedStream {
    parts: Arc<Mutex<VecDeque<Result<StreamPart, AiMuxError>>>>,
}

impl Stream for FeedStream {
    type Item = Result<StreamPart, AiMuxError>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.parts.lock().unwrap().pop_front() {
            Some(part) => Poll::Ready(Some(part)),
            None => Poll::Pending,
        }
    }
}

/// One poll of the next chunk, so a test can hold a wait open while it advances
/// the clock and feeds a part, then poll again where the idle deadline decides.
fn poll_once(stream: &mut ModelStream) -> Poll<Option<Result<ModelChunk, HostError>>> {
    let mut next = std::pin::pin!(stream.next());
    let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
    next.as_mut().poll(&mut context)
}

fn start() -> StreamPart {
    StreamPart::StreamStart { warnings: vec![] }
}

fn finish(reason: FinishReasonUnified, usage: NativeUsage) -> StreamPart {
    StreamPart::Finish {
        finish_reason: FinishReason {
            unified: reason,
            raw: None,
        },
        usage,
        provider_metadata: None,
    }
}

fn call(id: &str, name: &str, input: &str) -> RawToolCall {
    RawToolCall {
        tool_call_id: id.into(),
        tool_name: name.into(),
        input: input.into(),
        provider_executed: None,
        dynamic: None,
        provider_metadata: None,
    }
}

fn tool(input: &str) -> StreamPart {
    StreamPart::ToolCall(call("call-1", "lookup", input))
}

fn tool_start() -> StreamPart {
    StreamPart::ToolInputStart {
        id: "call-1".into(),
        tool_name: "lookup".into(),
        provider_executed: None,
        dynamic: None,
        title: None,
        provider_metadata: None,
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn native_content_and_tools_convert_in_order_to_agent_chunks() {
    let (mut stream, _, _, dropped) = fixture(vec![
        Ok(StreamPart::StreamStart {
            warnings: vec![Warning::Other {
                message: "diagnostic".into(),
            }],
        }),
        Ok(StreamPart::ResponseMetadata(
            aimux_core::types::ResponseMetadata {
                id: Some("provider-id".into()),
                timestamp: None,
                model_id: None,
            },
        )),
        Ok(StreamPart::ReasoningStart {
            id: "reasoning".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::ReasoningDelta {
            id: "reasoning".into(),
            delta: "thinking".into(),
            provider_metadata: Some(
                aimux_core::shared::provider_namespace("openai", json!({"new_key":true})).unwrap(),
            ),
        }),
        Ok(StreamPart::ReasoningEnd {
            id: "reasoning".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextStart {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextDelta {
            id: "text".into(),
            delta: String::new(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextDelta {
            id: "text".into(),
            delta: "answer".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextEnd {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::ToolInputStart {
            id: "call-1".into(),
            tool_name: "lookup".into(),
            provider_executed: None,
            dynamic: Some(true),
            title: Some("display-only".into()),
            provider_metadata: None,
        }),
        Ok(StreamPart::ToolInputDelta {
            id: "call-1".into(),
            delta: "{\"query\":\"test\"}".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::ToolInputEnd {
            id: "call-1".into(),
            provider_metadata: None,
        }),
        Ok(tool("{\"query\":\"test\"}")),
        Ok(StreamPart::ToolCall(call("call-2", "lookup", "{}"))),
        Ok(StreamPart::Raw {
            raw_value: json!({"ignored":"raw payload"}),
        }),
        Ok(finish(
            FinishReasonUnified::ToolCalls,
            NativeUsage::default(),
        )),
    ]);
    let ModelChunk::MessageStart { message_id } = stream.next().await.unwrap().unwrap() else {
        panic!("missing start")
    };
    assert_ne!(message_id, "provider-id");
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::Thinking(text) if text == "thinking")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ThinkingEnd { thinking } if thinking == "thinking")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::Text(text) if text == "answer")
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelChunk::TextEnd
    ));
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ToolStart { id, name } if id == "call-1" && name == "lookup")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ToolArguments { id, name, delta } if id == "call-1" && name == "lookup" && delta == "{\"query\":\"test\"}")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ToolUse(call) if call.id == "call-1" && call.name == "lookup" && call.input == json!({"query":"test"}).as_object().unwrap().clone())
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ToolStart { id, name } if id == "call-2" && name == "lookup")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::ToolUse(call) if call.id == "call-2" && call.input.is_empty())
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::MessageStop { message_id: id, reason: ModelStopReason::ToolUse } if id == message_id)
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn finish_releases_transport_before_usage_and_survives_late_cancel() {
    let usage = NativeUsage {
        input_tokens: InputTokenUsage {
            total: Some(u32::MAX),
            cache_read: Some(0),
            ..Default::default()
        },
        output_tokens: OutputTokenUsage {
            total: Some(u32::MAX),
            ..Default::default()
        },
        raw: Some(json!({"ignored":"raw usage"}).as_object().cloned().unwrap()),
    };
    let (mut stream, cancel, _, dropped) = fixture(vec![
        Ok(start()),
        Ok(StreamPart::TextStart {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextDelta {
            id: "text".into(),
            delta: "answer".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextEnd {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(finish(FinishReasonUnified::Stop, usage)),
    ]);
    let ModelChunk::MessageStart { message_id } = stream.next().await.unwrap().unwrap() else {
        panic!("missing start")
    };
    let ModelChunk::Text(delta) = stream.next().await.unwrap().unwrap() else {
        panic!("missing text")
    };
    assert_eq!(delta, "answer");
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelChunk::TextEnd
    ));
    let ModelChunk::Usage(usage) = stream.next().await.unwrap().unwrap() else {
        panic!("missing usage")
    };
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(usage.prompt_tokens, f64::from(u32::MAX));
    assert_eq!(usage.completion_tokens, f64::from(u32::MAX));
    assert_eq!(usage.total_tokens, 8_589_934_590.0);
    assert_eq!(usage.cache_read_tokens, Some(0.0));
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.cost, None);
    assert_eq!(usage.currency, None);
    cancel.cancel();
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ModelChunk::MessageStop { message_id: id, reason: ModelStopReason::EndTurn } if id == message_id)
    );
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn missing_usage_is_distinct_from_zero_and_length_remains_max_tokens() {
    for total in [None, Some(0)] {
        let usage = NativeUsage {
            input_tokens: InputTokenUsage {
                total,
                ..Default::default()
            },
            output_tokens: OutputTokenUsage {
                total,
                ..Default::default()
            },
            raw: None,
        };
        let (stream, _, _, _) = fixture(vec![
            Ok(start()),
            Ok(finish(FinishReasonUnified::Length, usage)),
        ]);
        let chunks = stream.collect::<Vec<_>>().await;
        let actual_usage = chunks.iter().find_map(|chunk| match chunk {
            Ok(ModelChunk::Usage(usage)) => Some(usage.total_tokens),
            _ => None,
        });
        assert_eq!(actual_usage, total.map(f64::from));
        assert!(matches!(
            chunks.last(),
            Some(Ok(ModelChunk::MessageStop {
                reason: ModelStopReason::MaxTokens,
                ..
            }))
        ));
    }
}

/// A reply that completes with neither non-whitespace text nor a tool call is
/// the one failure the converter names itself: the provider called it a
/// success and there is nothing for the turn to say.
#[tokio::test]
async fn a_completed_reply_with_no_answer_is_a_malformed_response() {
    let text = |content: &str| {
        vec![
            Ok(StreamPart::TextStart {
                id: "text".into(),
                provider_metadata: None,
            }),
            Ok(StreamPart::TextDelta {
                id: "text".into(),
                delta: content.into(),
                provider_metadata: None,
            }),
            Ok(StreamPart::TextEnd {
                id: "text".into(),
                provider_metadata: None,
            }),
        ]
    };
    let thinking = || {
        vec![
            Ok(StreamPart::ReasoningStart {
                id: "thinking".into(),
                provider_metadata: None,
            }),
            Ok(StreamPart::ReasoningDelta {
                id: "thinking".into(),
                delta: "thought".into(),
                provider_metadata: None,
            }),
            Ok(StreamPart::ReasoningEnd {
                id: "thinking".into(),
                provider_metadata: None,
            }),
        ]
    };
    let stop = || Ok(finish(FinishReasonUnified::Stop, NativeUsage::default()));
    let cases: Vec<(Vec<Result<StreamPart, AiMuxError>>, &str)> = vec![
        (vec![Ok(start()), stop()], "a reply with no part at all"),
        (
            {
                let mut parts = vec![Ok(start())];
                parts.extend(text(" \n\t"));
                parts.push(stop());
                parts
            },
            "whitespace text",
        ),
        (
            {
                let mut parts = vec![Ok(start())];
                parts.extend(thinking());
                parts.push(stop());
                parts
            },
            "thinking only",
        ),
    ];
    for (parts, case) in cases {
        let failure = collect_or_stall(fixture(parts).0, case)
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap_or_else(|| panic!("{case}: the reply completed"));
        let HostError::Failed {
            model: Some(model), ..
        } = failure
        else {
            panic!("{case}: {failure:?}");
        };
        assert_eq!(model.kind, ModelFailureKind::MalformedResponse, "{case}");
        assert_eq!(
            model.reason.as_deref(),
            Some("the response had no text and no tool call"),
            "{case}"
        );
    }
    // A reply with text, and one with a tool call only, complete as before.
    for (parts, case) in [
        (
            {
                let mut parts = vec![Ok(start())];
                parts.extend(text("answer"));
                parts.push(stop());
                parts
            },
            "text without a tool call",
        ),
        (
            vec![
                Ok(start()),
                Ok(tool("{}")),
                Ok(finish(
                    FinishReasonUnified::ToolCalls,
                    NativeUsage::default(),
                )),
            ],
            "a tool call without text",
        ),
    ] {
        let chunks = collect_or_stall(fixture(parts).0, case).await;
        assert!(
            matches!(chunks.last(), Some(Ok(ModelChunk::MessageStop { .. }))),
            "{case}: {chunks:?}"
        );
    }
}

#[tokio::test]
async fn cancellation_wins_ready_output_and_releases_pending_transport() {
    for ready in [false, true] {
        let mut parts = vec![Ok(start())];
        if ready {
            parts.push(Ok(finish(
                FinishReasonUnified::Stop,
                NativeUsage::default(),
            )));
        }
        let (mut stream, cancel, abort, dropped) = fixture(parts);
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelChunk::MessageStart { .. }
        ));
        cancel.cancel();
        assert!(matches!(
            stream.next().await,
            Some(Err(HostError::Cancelled(_)))
        ));
        assert!(abort.is_aborted());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(stream.next().await.is_none());
    }
}

#[tokio::test]
async fn errors_release_transport_and_do_not_expose_request_or_tool_input() {
    let secret = "private input text";
    let mut api_error = aimux_core::error::ApiCallError::new(
        "Rate limited",
        "https://test.invalid",
        json!({"secret":secret}),
    )
    .with_origin(aimux_core::ApiCallErrorOrigin::HttpResponse);
    api_error.status_code = Some(429);
    api_error.provider_code = Some("rate_limit_exceeded".into());
    api_error.response_body = Some(secret.into());
    let cases = [
        (
            Err(AiMuxError::ApiCall(Box::new(api_error))),
            "rate limiting",
        ),
        (Ok(tool("[1, 2]")), "JSON object"),
        (
            Ok(StreamPart::Error {
                error: AiMuxError::invalid_response_data("Truncated model stream"),
            }),
            "Truncated",
        ),
        (
            Err(AiMuxError::Aborted("Upstream aborted independently".into())),
            "aborted",
        ),
    ];
    for (failure, expected) in cases {
        let (mut stream, cancel, abort, dropped) = fixture(vec![
            Ok(start()),
            failure,
            Ok(finish(FinishReasonUnified::Stop, NativeUsage::default())),
        ]);
        stream.next().await.unwrap().unwrap();
        let Some(Err(HostError::Failed {
            message, budget, ..
        })) = stream.next().await
        else {
            panic!("missing failure")
        };
        assert!(message.contains(expected), "{message}");
        assert!(!message.contains(secret));
        assert!(budget.is_none());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(abort.is_aborted());
        cancel.cancel();
        assert!(stream.next().await.is_none());
    }

    // A tool argument failure names the tool and never its input.
    let (mut stream, cancel, abort, dropped) = fixture(vec![
        Ok(start()),
        Ok(tool(secret)),
        Ok(finish(FinishReasonUnified::Stop, NativeUsage::default())),
    ]);
    stream.next().await.unwrap().unwrap();
    let Some(Err(HostError::Failed {
        message,
        model: Some(model),
        ..
    })) = stream.next().await
    else {
        panic!("missing failure")
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert!(
        model
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("'lookup'")),
        "{model:?}"
    );
    assert!(
        message.contains("invalid arguments for tool 'lookup'"),
        "{message}"
    );
    let boundary = format!("{message:?} {message} {model:?}");
    assert!(!boundary.contains(secret), "{boundary}");
    assert!(abort.is_aborted());
    assert!(dropped.load(Ordering::SeqCst));
    cancel.cancel();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn unsupported_parameters_and_results_end_the_call() {
    let mut provider_tool = tool("{}");
    if let StreamPart::ToolCall(call) = &mut provider_tool {
        call.provider_executed = Some(true);
    }
    let cases = [
        StreamPart::StreamStart {
            warnings: vec![Warning::Unsupported {
                feature: "top_k".into(),
                details: None,
            }],
        },
        provider_tool,
        StreamPart::Source(Source::Url {
            id: "source".into(),
            url: "https://example.test/source".into(),
            title: None,
            provider_metadata: None,
        }),
        finish(FinishReasonUnified::ContentFilter, NativeUsage::default()),
    ];
    for part in cases {
        let case = format!("{part:?}");
        let (mut stream, _, _, dropped) = fixture(vec![Ok(part)]);
        assert!(matches!(
            next_or_stall(&mut stream, &case).await,
            Some(Err(HostError::Failed { .. }))
        ));
        assert!(dropped.load(Ordering::SeqCst));
        assert!(
            next_or_stall(&mut stream, &case).await.is_none(),
            "{case}: the stream did not end after the failure"
        );
    }
}

/// A finish reason that fails the reply is the one failure no provider error
/// object describes, so the bridge names its kind from the reason alone: a
/// content filter is `content_filtered`, and a provider generation error or an
/// unrecognized reason is `unknown`.
#[tokio::test]
async fn finish_reason_failures_carry_their_kind() {
    for (reason, kind, reason_sentence, line) in [
        (
            FinishReasonUnified::ContentFilter,
            ModelFailureKind::ContentFiltered,
            "the stream ended on a content-filter finish reason",
            "The provider's content filter blocked the response: the stream ended on a content-filter finish reason. Rephrase the request.",
        ),
        (
            FinishReasonUnified::Error,
            ModelFailureKind::Unknown,
            "the provider reported a generation error",
            "The model request failed: the provider reported a generation error. See the details.",
        ),
        (
            FinishReasonUnified::Other,
            ModelFailureKind::Unknown,
            "the stream ended on an unsupported finish reason",
            "The model request failed: the stream ended on an unsupported finish reason. See the details.",
        ),
    ] {
        let case = reason_sentence;
        let (stream, _, abort, dropped) = fixture(vec![
            Ok(start()),
            Ok(finish(reason, NativeUsage::default())),
        ]);
        let failure = collect_or_stall(stream, case)
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap_or_else(|| panic!("{case}: the reply completed"));
        let HostError::Failed {
            message,
            model: Some(model),
            ..
        } = failure
        else {
            panic!("{case}: {failure:?}");
        };
        assert_eq!(model.kind, kind, "{case}");
        assert_eq!(model.reason.as_deref(), Some(reason_sentence), "{case}");
        assert_eq!(message, line, "{case}");
        assert!(abort.is_aborted(), "{case}");
        assert!(dropped.load(Ordering::SeqCst), "{case}");
    }
}

#[tokio::test]
async fn dropping_output_stream_releases_transport_without_another_poll() {
    let (mut stream, _, _, dropped) = fixture(vec![Ok(start())]);
    stream.next().await.unwrap().unwrap();
    assert!(!dropped.load(Ordering::SeqCst));
    drop(stream);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn malformed_tool_lifecycles_fail_and_release_transport() {
    // The lifecycles strict mode now rejects (a delta before its start, a
    // repeated start, a late end) are covered by the provider-level guard and
    // by `malformed_native_block_lifecycle_is_a_classified_failure`; what
    // stays here are the checks the adapter itself makes.
    let input_start = || StreamPart::ToolInputStart {
        id: "call-1".into(),
        tool_name: "lookup".into(),
        provider_executed: None,
        dynamic: None,
        title: None,
        provider_metadata: None,
    };
    let end = || StreamPart::ToolInputEnd {
        id: "call-1".into(),
        provider_metadata: None,
    };
    let cases = [
        (
            vec![tool("{}"), tool("{}")],
            "Duplicate or mismatched tool call",
        ),
        (
            vec![
                input_start(),
                finish(FinishReasonUnified::Stop, NativeUsage::default()),
            ],
            "incomplete blocks or tool calls",
        ),
        (
            vec![
                end(),
                finish(FinishReasonUnified::Stop, NativeUsage::default()),
            ],
            "incomplete blocks or tool calls",
        ),
    ];
    for (parts, diagnostic) in cases {
        let (stream, _, abort, dropped) = fixture(parts.into_iter().map(Ok).collect());
        let chunks = collect_or_stall(stream, &format!("the {diagnostic:?} lifecycle")).await;
        let Some(Err(failure)) = chunks.last() else {
            panic!("missing failure: {chunks:?}")
        };
        assert!(matches!(
            failure,
            HostError::Failed {
                kind: HostErrorKind::Other,
                ..
            }
        ));
        let HostError::Failed {
            model: Some(model), ..
        } = failure
        else {
            panic!("{failure:?}");
        };
        assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
        assert!(failure.to_string().contains(diagnostic), "{failure}");
        assert!(abort.is_aborted());
        assert!(dropped.load(Ordering::SeqCst));
    }
    // A tool call whose name disagrees with its started input is mismatched.
    let mut mismatched = call("call-1", "different", "{}");
    mismatched.tool_call_id = "call-1".into();
    let (stream, _, abort, _) = fixture(vec![
        Ok(input_start()),
        Ok(StreamPart::ToolCall(mismatched)),
    ]);
    let chunks = stream.collect::<Vec<_>>().await;
    let Some(Err(failure)) = chunks.last() else {
        panic!("missing failure: {chunks:?}")
    };
    assert!(
        failure
            .to_string()
            .contains("Duplicate or mismatched tool call"),
        "{failure}"
    );
    assert!(abort.is_aborted());
}

#[tokio::test]
async fn whole_tool_with_input_end_and_truncated_open_blocks_remain_valid() {
    let (stream, _, _, _) = fixture(vec![
        Ok(StreamPart::ToolInputEnd {
            id: "call-1".into(),
            provider_metadata: None,
        }),
        Ok(tool("{}")),
        Ok(finish(
            FinishReasonUnified::ToolCalls,
            NativeUsage::default(),
        )),
    ]);
    let chunks = stream.collect::<Vec<_>>().await;
    assert!(chunks.iter().all(Result::is_ok));
    assert!(matches!(&chunks[0], Ok(ModelChunk::ToolStart { .. })));
    assert!(matches!(&chunks[1], Ok(ModelChunk::ToolUse(_))));

    for reason in [FinishReasonUnified::Stop, FinishReasonUnified::Length] {
        let (stream, _, _, _) = fixture(vec![
            Ok(StreamPart::TextStart {
                id: "text".into(),
                provider_metadata: None,
            }),
            Ok(finish(reason, NativeUsage::default())),
        ]);
        let chunks = stream.collect::<Vec<_>>().await;
        if reason == FinishReasonUnified::Length {
            assert!(matches!(
                chunks.last(),
                Some(Ok(ModelChunk::MessageStop {
                    reason: ModelStopReason::MaxTokens,
                    ..
                }))
            ));
        } else {
            assert!(matches!(chunks.last(), Some(Err(_))));
        }
    }
}

/// An interrupted tool call keeps its parse failure when the stream reports no
/// finish reason at all: a stream that ends, an error part and a stream error
/// all keep the malformed response the call already caused.
#[tokio::test]
async fn an_interrupted_tool_call_keeps_its_failure_without_a_finish_reason() {
    let interrupted = || {
        vec![
            Ok(StreamPart::ToolInputStart {
                id: "call-1".into(),
                tool_name: "lookup".into(),
                provider_executed: None,
                dynamic: None,
                title: None,
                provider_metadata: None,
            }),
            Ok(tool("{\"query\":")),
        ]
    };
    let mut error_part = interrupted();
    error_part.push(Ok(StreamPart::Error {
        error: AiMuxError::invalid_response_data("the stream broke"),
    }));
    let mut stream_error = interrupted();
    stream_error.push(Err(AiMuxError::invalid_response_data(
        "the transport broke",
    )));
    for (case, parts) in [
        ("a stream that ends", interrupted()),
        ("an error part", error_part),
        ("a stream error", stream_error),
    ] {
        let (stream, _, abort, dropped) = fixture_ending(parts);
        let chunks = collect_or_stall(stream, case).await;
        let Some(Err(failure)) = chunks.last() else {
            panic!("{case}: missing failure: {chunks:?}")
        };
        let HostError::Failed {
            model: Some(model), ..
        } = failure
        else {
            panic!("{case}: {failure:?}")
        };
        assert_eq!(model.kind, ModelFailureKind::MalformedResponse, "{case}");
        assert_eq!(
            model.reason.as_deref(),
            Some("invalid arguments for tool 'lookup'"),
            "{case}"
        );
        assert!(abort.is_aborted(), "{case}");
        assert!(dropped.load(Ordering::SeqCst), "{case}");
    }
}

/// A content-filter finish reason the stream delivers behind a call whose
/// arguments it cut short is the reply's terminal: the failure is
/// `content_filtered`, and the interrupted call is never announced as a
/// completed call.
#[tokio::test]
async fn a_cut_tool_call_takes_a_content_filter_queued_behind_it() {
    let (stream, _, abort, dropped) = fixture_ending(vec![
        Ok(start()),
        Ok(StreamPart::ToolInputStart {
            id: "call-1".into(),
            tool_name: "lookup".into(),
            provider_executed: None,
            dynamic: None,
            title: None,
            provider_metadata: None,
        }),
        Ok(tool("{\"query\":")),
        Ok(finish(
            FinishReasonUnified::ContentFilter,
            NativeUsage::default(),
        )),
    ]);
    let chunks = stream.collect::<Vec<_>>().await;
    let Some(Err(failure)) = chunks.last() else {
        panic!("missing failure: {chunks:?}")
    };
    let HostError::Failed {
        model: Some(model), ..
    } = failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::ContentFiltered);
    assert!(
        !chunks
            .iter()
            .any(|chunk| matches!(chunk, Ok(ModelChunk::ToolUse(_)))),
        "the interrupted call is not announced as a completed call: {chunks:?}"
    );
    assert!(abort.is_aborted());
    assert!(dropped.load(Ordering::SeqCst));
}

/// A stream that stops after a call whose arguments cannot be parsed has no
/// terminal to explain the call, so the parse failure is final at once and the
/// transport is released instead of the request being held open.
#[tokio::test]
async fn a_stalled_stream_after_a_cut_tool_call_keeps_its_parse_failure() {
    let (mut stream, _, abort, dropped) = fixture(vec![
        Ok(start()),
        Ok(StreamPart::ToolInputStart {
            id: "call-1".into(),
            tool_name: "lookup".into(),
            provider_executed: None,
            dynamic: None,
            title: None,
            provider_metadata: None,
        }),
        Ok(tool("{\"query\":")),
    ]);
    let mut chunks = Vec::new();
    while let Some(next) = stream.next().now_or_never() {
        let Some(chunk) = next else { break };
        let failed = chunk.is_err();
        chunks.push(chunk);
        if failed {
            break;
        }
    }
    assert!(
        matches!(chunks.last(), Some(Err(_))),
        "the bridge waited for a terminal the stream never delivered: {chunks:?}"
    );
    let Some(Err(failure)) = chunks.last() else {
        unreachable!()
    };
    let HostError::Failed {
        model: Some(model), ..
    } = failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(
        model.reason.as_deref(),
        Some("invalid arguments for tool 'lookup'")
    );
    assert!(abort.is_aborted());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        stream
            .next()
            .now_or_never()
            .is_some_and(|next| next.is_none()),
        "the stream did not end after the failure"
    );
}

/// Cancelling after a cut tool call wins over a part the stream has already
/// delivered: the cut call settles as cancelled rather than as its parse
/// failure.
#[tokio::test]
async fn cancelling_a_cut_tool_call_wins_a_ready_part() {
    let cancel = CancellationToken::new();
    let abort = AbortSignal::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let stream_cancel = cancel.clone();
    let result = StreamResult {
        stream: Box::pin(async_stream::stream! {
            let _flag = flag;
            yield Ok(start());
            yield Ok(tool_start());
            yield Ok(tool("{\"query\":"));
            stream_cancel.cancel();
            yield Ok(StreamPart::TextDelta {
                id: "text".into(),
                delta: "late".into(),
                provider_metadata: None,
            });
            std::future::pending::<()>().await;
        }),
        request: None,
        response: None,
    };
    let mut stream = convert(
        result,
        ToolNames::default(),
        ModelProtocol::OpenaiChat,
        cancel,
        abort.clone(),
        CancellationToken::new(),
        tokio_util::task::TaskTracker::new().token(),
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelChunk::MessageStart { .. }
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelChunk::ToolStart { .. }
    ));
    let next = stream.next().await;
    assert!(
        matches!(next, Some(Err(HostError::Cancelled(_)))),
        "{next:?}"
    );
    assert!(abort.is_aborted());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(stream.next().await.is_none());
}

/// The real Responses adapter with strict streaming, the assembly every
/// request uses, delivers a cut client call and the content-filter terminal
/// that explains it next to each other: the strict stream holds the call
/// until `Finish`, so the bridge sees the terminal on its next read even
/// though the adapter queues the call and the terminal in separate events.
#[tokio::test]
async fn a_strict_responses_stream_delivers_a_cut_call_next_to_its_filter() {
    use std::sync::Mutex;

    use aimux_core::language_model::LanguageModel;
    use aimux_core::language_model_message::convert_to_language_model_prompt;
    use aimux_core::message::ModelMessage;
    use aimux_core::options::CallOptions;
    use aimux_providers::openai::{OpenAIProviderSettings, create_openai};
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Seen {
        ToolCall,
        Finish(FinishReasonUnified),
        Other,
    }

    struct Observed {
        inner: Pin<Box<dyn Stream<Item = Result<StreamPart, AiMuxError>> + Send>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Stream for Observed {
        type Item = Result<StreamPart, AiMuxError>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            let polled = this.inner.as_mut().poll_next(cx);
            if let Poll::Ready(Some(Ok(part))) = &polled {
                this.seen.lock().unwrap().push(match part {
                    StreamPart::ToolCall(_) => Seen::ToolCall,
                    StreamPart::Finish { finish_reason, .. } => Seen::Finish(finish_reason.unified),
                    _ => Seen::Other,
                });
            }
            polled
        }
    }

    let server = MockServer::start().await;
    let body = [
        json!({"type":"response.created","response":{"id":"resp-order","created_at":1,"model":"test-model"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"lookup","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","output_index":0,"delta":"{\"query\":"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"lookup","arguments":"{\"query\":"}}),
        json!({"type":"response.incomplete","response":{"id":"resp-order","incomplete_details":{"reason":"content_filter"},"usage":{"input_tokens":3,"output_tokens":2}}}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let model = create_openai(OpenAIProviderSettings {
        api_key: Some("test-key".into()),
        base_url: Some(format!("{}/v1", server.uri())),
        ..Default::default()
    })
    .unwrap()
    .responses("test-model");
    let mut options = CallOptions::new(
        convert_to_language_model_prompt(&[ModelMessage::user("hi")], None, None).unwrap(),
    );
    options.strict_stream = Some(true);
    let result = model.do_stream(&options).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stream = convert(
        StreamResult {
            stream: Box::pin(Observed {
                inner: result.stream,
                seen: seen.clone(),
            }),
            request: result.request,
            response: result.response,
        },
        ToolNames::default(),
        ModelProtocol::OpenaiResponses,
        CancellationToken::new(),
        AbortSignal::new(),
        CancellationToken::new(),
        tokio_util::task::TaskTracker::new().token(),
    );
    let chunks = stream.collect::<Vec<_>>().await;
    {
        let seen = seen.lock().unwrap();
        assert!(
            matches!(
                seen.as_slice(),
                [
                    ..,
                    Seen::ToolCall,
                    Seen::Finish(FinishReasonUnified::ContentFilter)
                ]
            ),
            "the terminal was not handed over right after the cut call: {seen:?}"
        );
    }
    assert!(
        !chunks
            .iter()
            .any(|chunk| matches!(chunk, Ok(ModelChunk::ToolUse(_)))),
        "the interrupted call is not announced as a completed call: {chunks:?}"
    );
    let Some(Err(failure)) = chunks.last() else {
        panic!("missing failure: {chunks:?}")
    };
    let HostError::Failed {
        model: Some(model), ..
    } = failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::ContentFiltered);
    server.verify().await;
}

#[tokio::test]
async fn a_text_block_can_reopen_after_its_end() {
    // Strict mode rejects an end for a block that never started, a repeated
    // start and a repeated end; what remains here is the adapter's own
    // behavior for a provider that reuses a block id.
    let text_start = || StreamPart::TextStart {
        id: "block".into(),
        provider_metadata: None,
    };
    let text_end = || StreamPart::TextEnd {
        id: "block".into(),
        provider_metadata: None,
    };
    let (stream, _, _, dropped) = fixture(vec![
        Ok(text_start()),
        Ok(StreamPart::TextDelta {
            id: "block".into(),
            delta: "first".into(),
            provider_metadata: None,
        }),
        Ok(text_end()),
        Ok(text_start()),
        Ok(StreamPart::TextDelta {
            id: "block".into(),
            delta: " second".into(),
            provider_metadata: None,
        }),
        Ok(text_end()),
        Ok(finish(FinishReasonUnified::Stop, NativeUsage::default())),
    ]);
    let chunks = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    let chunks: Vec<_> = chunks.into_iter().collect::<Result<_, _>>().unwrap();
    let text: Vec<_> = chunks
        .iter()
        .filter_map(|chunk| match chunk {
            ModelChunk::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, ["first", " second"]);
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, ModelChunk::TextEnd))
            .count(),
        2
    );
    assert!(matches!(
        chunks.last(),
        Some(ModelChunk::MessageStop { .. })
    ));
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn a_failure_kind_follows_one_precedence_rule() {
    use aimux_core::ApiCallErrorOrigin::{HttpResponse as Http, StreamEvent as Event, Transport};

    let cases: [(ApiCallErrorOrigin, Option<u16>, &[&str], ModelFailureKind); 22] = [
        // A condition code decides at any status.
        (
            Http,
            Some(400),
            &["context_length_exceeded"],
            ModelFailureKind::ContextTooLong,
        ),
        (
            Http,
            Some(400),
            &["server_busy"],
            ModelFailureKind::ServerError,
        ),
        (
            Http,
            Some(503),
            &["engine_overloaded"],
            ModelFailureKind::ServerError,
        ),
        (
            Http,
            Some(401),
            &["invalid_api_key"],
            ModelFailureKind::AuthRejected,
        ),
        // A non-retried condition wins over a retried one beside it.
        (
            Http,
            Some(429),
            &["insufficient_quota", "rate_limit_error"],
            ModelFailureKind::QuotaExhausted,
        ),
        // An error status decides once no condition does; a 2xx or 3xx one,
        // which says nothing about the kind, lets the class label decide.
        (
            Http,
            Some(422),
            &["api_error"],
            ModelFailureKind::InvalidRequest,
        ),
        (
            Http,
            Some(501),
            &["server_error", "api_error"],
            ModelFailureKind::InvalidRequest,
        ),
        (
            Http,
            Some(500),
            &["invalid_request_error"],
            ModelFailureKind::ServerError,
        ),
        (Http, Some(429), &["timeout"], ModelFailureKind::RateLimited),
        (Http, Some(422), &[], ModelFailureKind::InvalidRequest),
        (Http, Some(503), &[], ModelFailureKind::ServerError),
        (
            Http,
            Some(200),
            &["invalid_request_error"],
            ModelFailureKind::InvalidRequest,
        ),
        (
            Http,
            Some(200),
            &["api_error"],
            ModelFailureKind::ServerError,
        ),
        (Http, Some(200), &[], ModelFailureKind::Unknown),
        (Http, Some(302), &[], ModelFailureKind::Unknown),
        // A transport failure is a network failure once no condition decides.
        (Transport, None, &[], ModelFailureKind::Network),
        (Transport, None, &["api_error"], ModelFailureKind::Network),
        (
            Transport,
            None,
            &["rate_limit_exceeded"],
            ModelFailureKind::RateLimited,
        ),
        // A stream event's status is the provider's mapping of the event's own
        // strings, not a response status, so only its codes decide. The empty
        // row with 500 is OpenAI's default guess for an unmatched event.
        (Event, Some(500), &[], ModelFailureKind::Unknown),
        (
            Event,
            Some(400),
            &["invalid_request_error"],
            ModelFailureKind::InvalidRequest,
        ),
        (
            Event,
            Some(200),
            &["server_error"],
            ModelFailureKind::ServerError,
        ),
        (
            Event,
            Some(200),
            &["unrecognized_failure"],
            ModelFailureKind::Unknown,
        ),
    ];
    let mismatched: Vec<String> = cases
        .iter()
        .filter_map(|(origin, status, codes, want)| {
            let codes: Vec<Option<&str>> = codes.iter().copied().map(Some).collect();
            let got = classify_kind(*origin, *status, None, &codes);
            (got != *want)
                .then(|| format!("{origin:?} {status:?} {codes:?}: got {got:?}, want {want:?}"))
        })
        .collect();
    assert!(mismatched.is_empty(), "{mismatched:#?}");
}

#[test]
fn a_timeout_is_unknown_and_not_retried() {
    let failure = error(
        AiMuxError::Timeout("model response timed out".into()),
        &CancellationToken::new(),
    );
    let HostError::Failed {
        message,
        model: Some(model),
        ..
    } = failure
    else {
        panic!("unexpected failure: {failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::Unknown);
    assert_eq!(model.status, None);
    assert_eq!(model.cause, Vec::<String>::new());
    assert_eq!(model.reason.as_deref(), Some("model response timed out"));
    assert_eq!(
        message,
        "The model request failed: model response timed out. See the details."
    );
}

#[test]
fn a_deep_transport_chain_keeps_its_innermost_frame_as_the_reason() {
    let mut cause = aimux_core::SourceError {
        message: "frame 8".into(),
        cause: None,
    };
    for frame in (1..8).rev() {
        cause = aimux_core::SourceError {
            message: format!("frame {frame}"),
            cause: Some(Box::new(cause)),
        };
    }
    let detail = aimux_core::ApiCallError {
        status_code: None,
        is_retryable: true,
        cause: Some(ErrorCause::Source(cause)),
        origin: aimux_core::ApiCallErrorOrigin::Transport,
        ..aimux_core::ApiCallError::new("frame 0", "http://example.test/v1", serde_json::json!({}))
    };
    let failure = error(
        AiMuxError::ApiCall(Box::new(detail)),
        &CancellationToken::new(),
    );
    let HostError::Failed {
        model: Some(model), ..
    } = failure
    else {
        panic!("unexpected failure: {failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(model.reason.as_deref(), Some("frame 8"));
    assert_eq!(model.cause.len(), FAILURE_CAUSE_FRAMES);
    assert_eq!(model.cause.first().map(String::as_str), Some("frame 0"));
    assert_eq!(model.cause.last().map(String::as_str), Some("frame 5"));
}

#[test]
fn an_unknown_tool_names_the_tool_without_the_available_list() {
    let failure = error(
        AiMuxError::NoSuchTool {
            tool_name: "ghost".into(),
            available_tools: Some(vec!["read".into(), "write".into()]),
            tool_input: Some("private tool input".into()),
        },
        &CancellationToken::new(),
    );
    let HostError::Failed {
        message,
        model: Some(model),
        ..
    } = failure
    else {
        panic!("unexpected failure: {failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(
        model.reason.as_deref(),
        Some("the model called an unknown tool 'ghost'")
    );
    let boundary = format!("{model:?} {message}");
    assert!(boundary.contains("ghost"), "{boundary}");
    assert!(!boundary.contains("read"), "{boundary}");
    assert!(!boundary.contains("write"), "{boundary}");
    assert!(!boundary.contains("private tool input"), "{boundary}");
}

#[test]
fn a_repair_failure_classifies_as_its_inner_failure() {
    let inner = AiMuxError::InvalidToolInput {
        tool_name: "lookup".into(),
        tool_input: "private tool input".into(),
        cause: ErrorCause::Source(aimux_core::SourceError {
            message: "expected value".into(),
            cause: None,
        }),
    };
    let failure = error(
        AiMuxError::ToolCallRepair {
            original_error: Box::new(inner.clone()),
            cause: Box::new(inner.clone()),
        },
        &CancellationToken::new(),
    );
    let HostError::Failed {
        message,
        model: Some(model),
        ..
    } = failure
    else {
        panic!("unexpected failure: {failure:?}");
    };
    let bare = error(inner, &CancellationToken::new());
    let HostError::Failed {
        message: bare_message,
        model: Some(bare),
        ..
    } = bare
    else {
        panic!("unexpected failure: {bare:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(model.kind, bare.kind);
    assert_eq!(model.reason, bare.reason);
    assert_eq!(message, bare_message);
    assert!(!message.contains("private tool input"));
}

#[test]
fn provider_codes_and_request_ids_are_one_bounded_line() {
    let mut detail = aimux_core::ApiCallError::new(
        "request rejected",
        "https://test.invalid",
        serde_json::json!({}),
    );
    detail.status_code = Some(400);
    detail.provider_code = Some(format!("code\n{}", "p".repeat(1_000)));
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "x-request-id".into(),
        format!("first\r\n{}", "r".repeat(1_000)),
    );
    detail.response_headers = Some(headers);
    let failure = error(
        AiMuxError::ApiCall(Box::new(detail)),
        &CancellationToken::new(),
    );
    let HostError::Failed {
        model: Some(model), ..
    } = failure
    else {
        panic!("unexpected failure: {failure:?}");
    };
    let provider_code = model.provider_code.expect("a code");
    assert!(!provider_code.contains(['\n', '\r']), "{provider_code}");
    assert!(
        provider_code.chars().count() <= FAILURE_CAUSE_CHARS,
        "{provider_code}"
    );
    assert!(provider_code.starts_with("code "), "{provider_code}");
    let request_id = model.request_id.expect("a request id");
    assert!(!request_id.contains(['\n', '\r']), "{request_id}");
    assert!(
        request_id.chars().count() <= FAILURE_CAUSE_CHARS,
        "{request_id}"
    );
    assert!(request_id.starts_with("first "), "{request_id}");
}

/// A recorded Chat Completions usage carrying
/// `completion_tokens_details.reasoning_tokens` reaches the agent usage with
/// its reasoning share.
#[tokio::test]
async fn recorded_reasoning_tokens_reach_the_agent_usage() {
    let usage = NativeUsage {
        input_tokens: InputTokenUsage {
            total: Some(3),
            ..Default::default()
        },
        output_tokens: OutputTokenUsage {
            total: Some(6),
            text: Some(4),
            reasoning: Some(2),
        },
        raw: None,
    };
    let (stream, _, _, _) = fixture(vec![
        Ok(start()),
        Ok(StreamPart::TextStart {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextDelta {
            id: "text".into(),
            delta: "answer".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextEnd {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(finish(FinishReasonUnified::Stop, usage)),
    ]);
    let recorded = collect_or_stall(stream, "recorded reasoning")
        .await
        .into_iter()
        .find_map(|chunk| match chunk {
            Ok(ModelChunk::Usage(usage)) => Some(serde_json::to_value(&usage).unwrap()),
            _ => None,
        })
        .expect("the reply's usage");
    assert_eq!(recorded["completionTokens"].as_f64(), Some(6.0));
    assert_eq!(
        recorded["reasoningTokens"].as_f64(),
        Some(2.0),
        "{recorded}"
    );
}

/// The documented allowance: five minutes without a part is a lost connection.
const IDLE_ALLOWANCE: Duration = Duration::from_secs(300);

/// A text block and one delta of it, as the parts a provider delivers.
fn text_part(delta: &str) -> Vec<Result<StreamPart, AiMuxError>> {
    vec![
        Ok(StreamPart::TextStart {
            id: "text".into(),
            provider_metadata: None,
        }),
        Ok(StreamPart::TextDelta {
            id: "text".into(),
            delta: delta.into(),
            provider_metadata: None,
        }),
    ]
}

/// The idle failure of one poll, asserted to carry the network kind and the
/// facts the retry policy reads.
fn idle_failure(
    outcome: Poll<Option<Result<ModelChunk, HostError>>>,
) -> (String, ModelFailureFacts) {
    let Poll::Ready(Some(Err(HostError::Failed {
        kind,
        message,
        model,
        ..
    }))) = outcome
    else {
        panic!("a stream silent for the idle allowance fails");
    };
    assert_eq!(kind, HostErrorKind::Transport);
    let facts = model.expect("the idle failure carries facts");
    assert_eq!(facts.kind, ModelFailureKind::Network);
    (message, *facts)
}

/// Five minutes without a part is a lost connection: the request fails as a
/// network failure, which the retry policy retries while nothing was committed.
#[tokio::test(start_paused = true)]
async fn a_stream_silent_for_the_idle_allowance_fails_as_a_network_failure() {
    let (mut stream, feed) = feedable();
    feed.push(vec![Ok(start())]);
    assert!(matches!(
        poll_once(&mut stream),
        Poll::Ready(Some(Ok(ModelChunk::MessageStart { .. })))
    ));
    let mut waiting = std::pin::pin!(stream.next());
    let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(waiting.as_mut().poll(&mut context).is_pending());
    tokio::time::advance(IDLE_ALLOWANCE).await;
    let (message, facts) = idle_failure(waiting.as_mut().poll(&mut context));
    assert_eq!(
        facts.reason.as_deref(),
        Some("the stream delivered nothing for 300 seconds")
    );
    assert!(
        message.ends_with(ModelFailureKind::Network.action()),
        "{message}"
    );
}

/// A part that arrives inside the allowance restarts it, so a reply that keeps
/// streaming is never cut however long it takes.
#[tokio::test(start_paused = true)]
async fn a_part_inside_the_idle_allowance_keeps_the_stream_open() {
    let (mut stream, feed) = feedable();
    feed.push(vec![Ok(start())]);
    assert!(matches!(
        poll_once(&mut stream),
        Poll::Ready(Some(Ok(ModelChunk::MessageStart { .. })))
    ));
    let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
    let inside = IDLE_ALLOWANCE - Duration::from_millis(1);
    for delta in ["first", "second"] {
        let mut waiting = std::pin::pin!(stream.next());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        tokio::time::advance(inside).await;
        feed.push(text_part(delta));
        assert!(matches!(
            waiting.as_mut().poll(&mut context),
            Poll::Ready(Some(Ok(ModelChunk::Text(text)))) if text == delta
        ));
    }
}

/// A deadline that has passed outranks a part ready at the same moment: the
/// connection was silent for the whole allowance even though data arrived.
#[tokio::test(start_paused = true)]
async fn an_idle_deadline_outranks_a_part_that_is_ready_at_the_same_time() {
    let (mut stream, feed) = feedable();
    feed.push(vec![Ok(start())]);
    assert!(matches!(
        poll_once(&mut stream),
        Poll::Ready(Some(Ok(ModelChunk::MessageStart { .. })))
    ));
    let mut waiting = std::pin::pin!(stream.next());
    let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(waiting.as_mut().poll(&mut context).is_pending());
    feed.push(text_part("ready"));
    tokio::time::advance(IDLE_ALLOWANCE).await;
    idle_failure(waiting.as_mut().poll(&mut context));
}

/// One provider call failure: the origin and status aimux reports, the error
/// data the provider sent and the code aimux normalizes separately.
fn call_failure(
    origin: aimux_core::ApiCallErrorOrigin,
    status: Option<u16>,
    data: serde_json::Value,
    provider_code: Option<&str>,
) -> AiMuxError {
    let mut detail = aimux_core::ApiCallError::new(
        "request failed",
        "https://test.invalid",
        serde_json::json!({}),
    )
    .with_origin(origin);
    detail.status_code = status;
    detail.data = Some(data);
    detail.provider_code = provider_code.map(str::to_owned);
    AiMuxError::ApiCall(Box::new(detail))
}

/// The classified facts of one failure.
fn classified(failure: AiMuxError) -> (HostErrorKind, ModelFailureFacts) {
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = error(failure, &CancellationToken::new())
    else {
        panic!("a classified failure carries facts");
    };
    (kind, *model)
}

/// The status a stream event writes as its own code names a class, the same
/// way a response status does. It decides after a condition, after a
/// transport failure and after a response's own status, and before the
/// unknown kind.
#[test]
fn a_stream_event_status_decides_after_a_condition_and_a_response_status() {
    use aimux_core::ApiCallErrorOrigin::{HttpResponse as Http, StreamEvent as Event, Transport};

    type Case = (
        ApiCallErrorOrigin,
        Option<u16>,
        Option<u16>,
        &'static [&'static str],
        ModelFailureKind,
    );
    let cases: [Case; 13] = [
        // The event's own code decides, not the status aimux guessed for it.
        (
            Event,
            Some(500),
            Some(502),
            &[],
            ModelFailureKind::ServerError,
        ),
        (
            Event,
            Some(500),
            Some(504),
            &[],
            ModelFailureKind::ServerError,
        ),
        (
            Event,
            Some(500),
            Some(408),
            &[],
            ModelFailureKind::ServerError,
        ),
        (
            Event,
            Some(500),
            Some(429),
            &[],
            ModelFailureKind::RateLimited,
        ),
        (
            Event,
            Some(500),
            Some(401),
            &[],
            ModelFailureKind::AuthRejected,
        ),
        (
            Event,
            Some(500),
            Some(403),
            &[],
            ModelFailureKind::AuthRejected,
        ),
        // A condition beside the event's own status still decides.
        (
            Event,
            Some(500),
            Some(429),
            &["insufficient_quota"],
            ModelFailureKind::QuotaExhausted,
        ),
        // The event's own status outranks a named class label beside it.
        (
            Event,
            Some(500),
            Some(502),
            &["authentication_error"],
            ModelFailureKind::ServerError,
        ),
        (Event, Some(500), None, &[], ModelFailureKind::Unknown),
        // A transport failure stays a network failure once no condition
        // decides.
        (
            Transport,
            None,
            None,
            &["rate_limit_exceeded"],
            ModelFailureKind::RateLimited,
        ),
        (
            Transport,
            None,
            None,
            &["api_error"],
            ModelFailureKind::Network,
        ),
        // A response's own status decides before its body's class label.
        (
            Http,
            Some(422),
            None,
            &["api_error"],
            ModelFailureKind::InvalidRequest,
        ),
        (
            Http,
            Some(200),
            None,
            &["api_error"],
            ModelFailureKind::ServerError,
        ),
    ];
    let mismatched: Vec<String> = cases
        .iter()
        .filter_map(|(origin, status, code_status, codes, want)| {
            let codes: Vec<Option<&str>> = codes.iter().copied().map(Some).collect();
            let got = classify_kind(*origin, *status, *code_status, &codes);
            (got != *want).then(|| {
                format!(
                    "{origin:?} {status:?} {code_status:?} {codes:?}: got {got:?}, want {want:?}"
                )
            })
        })
        .collect();
    assert!(mismatched.is_empty(), "{mismatched:#?}");
}

/// A stream event that writes its status as a JSON integer is read through
/// the same three digits, in its own shape or one level down in the response
/// envelope, and a number outside 400 to 599 is not read as a code at all.
#[test]
fn a_stream_event_status_written_as_a_number_is_a_class_label() {
    use aimux_core::ApiCallErrorOrigin::StreamEvent as Event;

    let (kind, facts) = classified(call_failure(
        Event,
        Some(502),
        json!({"error": {"code": 502, "message": "Network connection lost."}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::ServerError);
    assert_eq!(
        kind,
        HostErrorKind::Http {
            status: Some(502),
            code: Some("502".into()),
        }
    );

    // A provider that writes the status as a number and names its own status
    // word as the provider code is read from the number.
    let (_, facts) = classified(call_failure(
        Event,
        Some(503),
        json!({"error": {"code": 503, "message": "The model is overloaded.", "status": "UNAVAILABLE"}}),
        Some("unavailable"),
    ));
    assert_eq!(facts.kind, ModelFailureKind::ServerError);
    assert_eq!(facts.provider_code.as_deref(), Some("unavailable"));

    let (_, facts) = classified(call_failure(
        Event,
        Some(429),
        json!({"error": {"code": 429, "message": "Rate limit exceeded."}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::RateLimited);

    // The response envelope carries the number one level down, and a status
    // aimux guessed for the event does not decide the kind.
    let (_, facts) = classified(call_failure(
        Event,
        Some(500),
        json!({"type": "response.failed", "response": {"error": {"code": 504, "message": "Gateway timeout."}}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::ServerError);

    // A number outside the status classes decides nothing, and the code it
    // was read from stays empty: the status aimux guessed for the event does
    // not decide the kind either.
    for code in [399, 600, 1000] {
        let (kind, facts) = classified(call_failure(
            Event,
            Some(500),
            json!({"error": {"code": code, "message": "Unrecognized."}}),
            None,
        ));
        assert_eq!(facts.kind, ModelFailureKind::Unknown, "{code}");
        assert_eq!(
            kind,
            HostErrorKind::Http {
                status: Some(500),
                code: None,
            },
            "{code}"
        );
    }
}

/// Only a stream event writes its status as a number: a response's own status
/// decides, and a number its body carries is not read.
#[test]
fn a_response_status_outranks_the_status_code_in_its_body() {
    use aimux_core::ApiCallErrorOrigin::HttpResponse as Http;

    let (kind, facts) = classified(call_failure(
        Http,
        Some(422),
        json!({"error": {"code": "500", "message": "the upstream refused the request's shape"}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::InvalidRequest);
    assert_eq!(
        kind,
        HostErrorKind::Http {
            status: Some(422),
            code: Some("500".into()),
        }
    );

    let (_, facts) = classified(call_failure(
        Http,
        Some(200),
        json!({"error": {"code": 500, "message": "refused"}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::Unknown);
}

/// A three-digit number is a status only where a stream event writes it as
/// its own code: an opaque numeric `type` or provider code keeps only its
/// named classes, and an unknown one stays unknown.
#[test]
fn a_numeric_type_or_provider_code_is_not_a_status() {
    use aimux_core::ApiCallErrorOrigin::StreamEvent as Event;

    for label in ["502", "504"] {
        let (kind, facts) = classified(call_failure(
            Event,
            Some(500),
            json!({"error": {"type": label, "message": "the provider reported an opaque label"}}),
            None,
        ));
        assert_eq!(facts.kind, ModelFailureKind::Unknown, "{label}");
        assert_eq!(
            kind,
            HostErrorKind::Http {
                status: Some(500),
                code: None,
            },
            "{label}"
        );
    }

    let (kind, facts) = classified(call_failure(
        Event,
        Some(500),
        json!({"error": {"message": "the provider reported an opaque label"}}),
        Some("502"),
    ));
    assert_eq!(facts.kind, ModelFailureKind::Unknown);
    assert_eq!(facts.provider_code.as_deref(), Some("502"));
    assert_eq!(
        kind,
        HostErrorKind::Http {
            status: Some(500),
            code: None,
        }
    );
}

/// A stream event that writes its status as an exactly three-digit decimal
/// string is read through the same status table as the JSON integer, and its
/// `code` keeps the string.
#[test]
fn a_stream_event_status_written_as_a_string_is_a_class_label() {
    use aimux_core::ApiCallErrorOrigin::StreamEvent as Event;

    for (code, want) in [
        ("502", ModelFailureKind::ServerError),
        ("503", ModelFailureKind::ServerError),
        ("429", ModelFailureKind::RateLimited),
        ("401", ModelFailureKind::AuthRejected),
    ] {
        let (kind, facts) = classified(call_failure(
            Event,
            Some(500),
            json!({"error": {"code": code, "message": "the provider reported the failure"}}),
            None,
        ));
        assert_eq!(facts.kind, want, "{code}");
        assert_eq!(
            kind,
            HostErrorKind::Http {
                status: Some(500),
                code: Some(code.into()),
            },
            "{code}"
        );
    }
}

/// A string code is a status only in a stream event's own code, exactly three
/// ASCII digits and inside 400 to 599: anything else keeps the normalized
/// string as an opaque code, and a response's own status still decides.
#[test]
fn a_string_code_is_a_status_only_when_it_is_exactly_three_digits_in_range() {
    use aimux_core::ApiCallErrorOrigin::{HttpResponse as Http, StreamEvent as Event};

    for code in ["399", "600", "0502"] {
        let (kind, facts) = classified(call_failure(
            Event,
            Some(500),
            json!({"error": {"code": code, "message": "Unrecognized."}}),
            None,
        ));
        assert_eq!(facts.kind, ModelFailureKind::Unknown, "{code}");
        assert_eq!(
            kind,
            HostErrorKind::Http {
                status: Some(500),
                code: Some(code.into()),
            },
            "{code}"
        );
    }

    // The extraction itself yields no status outside 400 to 599, in either
    // form.
    for code in [json!(399), json!(600), json!("399"), json!("600")] {
        assert_eq!(stream_event_status(&code), None, "{code}");
    }

    let (kind, facts) = classified(call_failure(
        Http,
        Some(200),
        json!({"error": {"code": "500", "message": "refused"}}),
        None,
    ));
    assert_eq!(facts.kind, ModelFailureKind::Unknown);
    assert_eq!(
        kind,
        HostErrorKind::Http {
            status: Some(200),
            code: Some("500".into()),
        }
    );
}
