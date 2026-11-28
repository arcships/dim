use std::{
    collections::VecDeque,
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    task::{Wake, Waker},
};

use axum::body::Bytes;
use dim_agent_types::{Message, ModelFailureKind, Role};
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};

use super::*;

/// A transport the test answers itself: the exchange waits for the reply the
/// test releases, and the reply's body yields the bytes the test pushes.
struct ScriptedFetch {
    reply: Mutex<Option<tokio::sync::oneshot::Receiver<ScriptedReply>>>,
}

#[derive(Debug)]
struct ScriptedReply {
    status: http::StatusCode,
    headers: http::HeaderMap,
    body: ScriptedBody,
}

#[async_trait::async_trait]
impl Fetch for ScriptedFetch {
    async fn fetch(&self, _: FetchRequest) -> Result<FetchResponse, FetchError> {
        let reply = self
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("one scripted reply per exchange");
        let reply = reply.await.expect("the scripted reply is released");
        Ok(FetchResponse {
            status: reply.status,
            headers: reply.headers,
            url: url::Url::parse("http://aimux.test/v1").unwrap(),
            body: reply.body.stream(),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct ScriptedBody {
    chunks: Arc<Mutex<VecDeque<Bytes>>>,
}

impl ScriptedBody {
    fn push(&self, chunk: impl Into<Bytes>) {
        self.chunks.lock().unwrap().push_back(chunk.into());
    }
    fn stream(self) -> BoxStream<'static, Result<Bytes, FetchError>> {
        Box::pin(ScriptedBodyStream(self))
    }
}

struct ScriptedBodyStream(ScriptedBody);

impl Stream for ScriptedBodyStream {
    type Item = Result<Bytes, FetchError>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().0.chunks.lock().unwrap().pop_front() {
            Some(chunk) => Poll::Ready(Some(Ok(chunk))),
            None => Poll::Pending,
        }
    }
}

fn scripted() -> (
    Arc<ScriptedFetch>,
    tokio::sync::oneshot::Sender<ScriptedReply>,
) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    (
        Arc::new(ScriptedFetch {
            reply: Mutex::new(Some(receiver)),
        }),
        sender,
    )
}

fn driver(fetch: Arc<ScriptedFetch>) -> AimuxDriver {
    AimuxDriver {
        files: None,
        identity: ModelIdentity::default(),
        transport: ReqwestFetch::default(),
        fetch,
        generation: CancellationToken::new(),
        tasks: TaskTracker::new(),
        streams: Mutex::new(Vec::new()),
        requests: tokio::sync::Semaphore::new(4),
    }
}

fn driver_with(fetch: FetchFunction) -> AimuxDriver {
    AimuxDriver {
        files: None,
        identity: ModelIdentity::default(),
        transport: ReqwestFetch::default(),
        fetch,
        generation: CancellationToken::new(),
        tasks: TaskTracker::new(),
        streams: Mutex::new(Vec::new()),
        requests: tokio::sync::Semaphore::new(4),
    }
}

/// A transport whose reply body is already closed: an exchange that reads a
/// non-2xx body completes in the poll that produced the headers.
struct EndedBodyFetch {
    reply: Mutex<Option<tokio::sync::oneshot::Receiver<EndedBodyReply>>>,
}

#[derive(Debug)]
struct EndedBodyReply {
    status: http::StatusCode,
    headers: http::HeaderMap,
}

#[async_trait::async_trait]
impl Fetch for EndedBodyFetch {
    async fn fetch(&self, _: FetchRequest) -> Result<FetchResponse, FetchError> {
        let reply = self
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("one scripted reply per exchange");
        let reply = reply.await.expect("the scripted reply is released");
        Ok(FetchResponse {
            status: reply.status,
            headers: reply.headers,
            url: url::Url::parse("http://aimux.test/v1").unwrap(),
            body: futures_util::stream::empty::<Result<Bytes, FetchError>>().boxed(),
        })
    }
}

/// A transport the test fails itself: the exchange reports the error the test
/// releases, and no response headers were produced to report.
struct FailingFetch {
    reply: Mutex<Option<tokio::sync::oneshot::Receiver<FetchError>>>,
}

#[async_trait::async_trait]
impl Fetch for FailingFetch {
    async fn fetch(&self, _: FetchRequest) -> Result<FetchResponse, FetchError> {
        let error = self
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("one scripted failure per exchange");
        Err(error.await.expect("the scripted failure is released"))
    }
}

fn request() -> ModelRequest {
    ModelRequest::new("local", "test-model", vec![Message::text(Role::User, "hi")])
}

fn connection(protocol: ModelProtocol) -> ModelConnection {
    ModelConnection::Http(HttpModelConnection {
        protocol,
        base_url: "http://aimux.test/v1".into(),
        network: Default::default(),
        headers: Default::default(),
        bearer: None,
        api_key: None,
        preset: None,
    })
}

fn headers(content_type: &'static str) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static(content_type),
    );
    headers
}

struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Polls `future` the way an executor would and stops at the first `Pending`
/// poll that woke nothing: the future then waits only for the test's next
/// signal, so the clock can move without skipping work that was already ready.
fn poll_until_idle<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Option<F::Output> {
    let flag = Arc::new(WakeFlag(AtomicBool::new(false)));
    let waker = Waker::from(flag.clone());
    let mut context = Context::from_waker(&waker);
    loop {
        flag.0.store(false, Ordering::SeqCst);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return Some(output),
            Poll::Pending if !flag.0.load(Ordering::SeqCst) => return None,
            Poll::Pending => {}
        }
    }
}

/// Polls `future` once and leaves any wake-up it registers to the next poll:
/// the task is not run again until the test says so.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(WakeFlag(AtomicBool::new(false))));
    future.poll(&mut Context::from_waker(&waker))
}

fn next_chunk(stream: &mut ModelStream) -> Option<Result<ModelChunk, HostError>> {
    let mut next = std::pin::pin!(stream.next());
    poll_until_idle(next.as_mut()).flatten()
}

/// Headers inside the header allowance and a first event after the whole
/// request allowance still starts: the wait for the headers and the wait for
/// the first event are bounded separately.
#[tokio::test(start_paused = true)]
async fn a_delayed_header_reply_still_gets_its_first_event_allowance() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the reply must wait for its first event"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the first event gets its own allowance after the headers"
    );
    body.push(format!(
        "data: {}\n\n",
        json!({"id": "remote-id", "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": Value::Null}]})
    ));
    let mut stream = poll_until_idle(invoke.as_mut())
        .expect("the first event must start the reply")
        .expect("the reply must start after the first event");
    let start = next_chunk(&mut stream)
        .expect("the reply must deliver its start")
        .expect("the reply must not fail before its start");
    assert!(matches!(start, ModelChunk::MessageStart { .. }));
    let text = next_chunk(&mut stream)
        .expect("the reply must deliver its text")
        .expect("the first event must deliver text");
    assert!(matches!(text, ModelChunk::Text(text) if text == "hello"));
}

/// Headers inside the allowance and no first event inside its own allowance:
/// the reply fails as a network failure naming the missing event, not the
/// headers.
#[tokio::test(start_paused = true)]
async fn a_first_event_past_its_own_allowance_fails_as_a_network_failure() {
    let (fetch, reply) = scripted();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: ScriptedBody::default(),
        })
        .unwrap();
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the reply must wait for its first event"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a reply without a first event must fail"),
        None => panic!("a reply without a first event must fail within the allowance"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("the response headers arrived, but no stream event followed within 300 seconds")
    );
}

/// A non-2xx response whose error body stalls is read under its own allowance:
/// the request fails with the HTTP status and the kind that status names, not
/// as an idle network failure that discards a known status.
#[tokio::test(start_paused = true)]
async fn a_stalled_error_body_keeps_the_response_status() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    body.push(r#"{"error":{"message":"Expired credential""#);
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiCompatible),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    reply
        .send(ScriptedReply {
            status: http::StatusCode::UNAUTHORIZED,
            headers: headers("application/json"),
            body,
        })
        .unwrap();
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the error body must hold the request open"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a 401 response must fail the request"),
        None => panic!("a stalled error body must fail within the allowance"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(
        *kind,
        HostErrorKind::Http {
            status: Some(401),
            code: None,
        }
    );
    assert_eq!(model.kind, ModelFailureKind::AuthRejected);
    assert_eq!(model.status, Some(401));
}

/// The transport produces the headers at 299 seconds but the task is not
/// polled again until after the header allowance: the report carries the
/// instant it was written, so the reply starts and its first event is
/// measured from that instant.
#[tokio::test(start_paused = true)]
async fn a_header_report_read_past_its_allowance_is_on_time() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    // One poll lets the transport report the headers at 299 seconds; the task
    // is not polled again until 301, past the header allowance.
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the headers"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "a report written inside the header allowance must not be read as late"
    );
    body.push(format!(
        "data: {}\n\n",
        json!({"id": "remote-id", "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": Value::Null}]})
    ));
    let mut stream = poll_until_idle(invoke.as_mut())
        .expect("the first event must start the reply")
        .expect("the reply must start after the first event");
    let start = next_chunk(&mut stream)
        .expect("the reply must deliver its start")
        .expect("the reply must not fail before its start");
    assert!(matches!(start, ModelChunk::MessageStart { .. }));
    let text = next_chunk(&mut stream)
        .expect("the reply must deliver its text")
        .expect("the first event must deliver text");
    assert!(matches!(text, ModelChunk::Text(text) if text == "hello"));
}

/// A 401 produced at 299 seconds and read after the header allowance keeps
/// its status: the report is on time, and the stalled error body still ends
/// under its own allowance.
#[tokio::test(start_paused = true)]
async fn a_header_report_read_past_its_allowance_keeps_the_response_status() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    body.push(r#"{"error":{"message":"Expired credential"#);
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiCompatible),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::UNAUTHORIZED,
            headers: headers("application/json"),
            body,
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the headers"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "a report written inside the header allowance must not become a timeout"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a 401 response must fail the request"),
        None => panic!("a stalled error body must fail within its allowance"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(
        *kind,
        HostErrorKind::Http {
            status: Some(401),
            code: None,
        }
    );
    assert_eq!(model.kind, ModelFailureKind::AuthRejected);
    assert_eq!(model.status, Some(401));
}

/// The stream allowance runs from the instant the transport reported the
/// headers, not from the poll that read the report: headers reported at 299
/// seconds and read at 301 end the wait at 599, not at 601.
#[tokio::test(start_paused = true)]
async fn a_header_report_starts_the_stream_allowance_at_its_instant() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body,
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the headers"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "a report written inside the header allowance must not be read as late"
    );
    tokio::time::advance(Duration::from_secs(298)).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a reply without a first event must fail"),
        None => panic!("the stream allowance must end 300 seconds after the report"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("the response headers arrived, but no stream event followed within 300 seconds")
    );
}

/// The OpenAI Responses startup scan decodes response metadata before it
/// returns the stream. The wait after the headers ends when the stream
/// starts, and its failure says so instead of reporting a missing event.
#[tokio::test(start_paused = true)]
async fn a_responses_startup_past_its_allowance_names_the_stream_start() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiResponses),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the reply must wait for the start of its stream"
    );
    body.push(format!(
        "data: {}\n\n",
        json!({"type": "response.created", "response": {"id": "resp-1", "created_at": 1, "model": "test-model"}})
    ));
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "decoded metadata must not start the stream"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a stream that never starts must fail"),
        None => panic!("a stream that never starts must fail within the allowance"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("the response headers arrived, but the stream did not start within 300 seconds")
    );
}

/// A Responses reply that decodes metadata first and delivers output inside
/// the allowance starts and delivers that output.
#[tokio::test(start_paused = true)]
async fn a_responses_start_inside_its_allowance_delivers_the_output() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiResponses),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the reply must wait for the start of its stream"
    );
    body.push(format!(
        "data: {}\n\n",
        json!({"type": "response.created", "response": {"id": "resp-1", "created_at": 1, "model": "test-model"}})
    ));
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "decoded metadata must not start the stream"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    for event in [
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "msg-1", "role": "assistant", "content": []}}),
        json!({"type": "response.output_text.delta", "item_id": "msg-1", "output_index": 0, "content_index": 0, "delta": "hello"}),
    ] {
        body.push(format!("data: {event}\n\n"));
    }
    let mut stream = poll_until_idle(invoke.as_mut())
        .expect("the output must start the reply")
        .expect("the reply must start after the output");
    let mut text = String::new();
    while let Some(chunk) = next_chunk(&mut stream) {
        if let ModelChunk::Text(part) = chunk.expect("the reply must not fail") {
            text.push_str(&part);
        }
    }
    assert_eq!(text, "hello");
}

/// The timer that ends the header allowance fires on its next millisecond
/// tick, so a report written just after the allowance can be read before the
/// timer fires. It is late by its own instant there too: the wait ends as
/// missing headers instead of starting a fresh window from a late report.
#[tokio::test(start_paused = true)]
async fn a_header_report_written_after_its_allowance_is_late() {
    let (fetch, reply) = scripted();
    let driver = driver(fetch);
    // Half a tick puts the allowance between two ticks, so the timer fires up
    // to one tick after it.
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    // The exchange is polled 0.2 ms after the allowance, before its timer.
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: ScriptedBody::default(),
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the report"
    );
    tokio::time::advance(Duration::from_micros(100)).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a reply must not start from a late report"),
        None => panic!("a report written after the allowance must fail before the timer fires"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("no response headers arrived within 300 seconds of sending the request")
    );
}

/// The same late report read after the timer tick: the deadline arm rejects a
/// report stamped after the allowance, so a report's classification does not
/// depend on which arm polls it first.
#[tokio::test(start_paused = true)]
async fn a_header_report_written_after_its_allowance_is_late_after_the_tick() {
    let (fetch, reply) = scripted();
    let driver = driver(fetch);
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: ScriptedBody::default(),
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the report"
    );
    tokio::time::advance(Duration::from_micros(600)).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a reply must not start from a late report"),
        None => panic!("the deadline arm must fail a late report"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("no response headers arrived within 300 seconds of sending the request")
    );
}

/// A 401 written after the header allowance is late as well: read before the
/// timer fires, it ends the wait as missing headers, the failure the deadline
/// arm gives the same report, and it does not keep its status.
#[tokio::test(start_paused = true)]
async fn a_late_error_report_fails_as_a_network_failure() {
    let (fetch, reply) = scripted();
    let driver = driver(fetch);
    // Half a tick puts the allowance between two ticks, so the timer fires up
    // to one tick after it.
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    // The exchange is polled 0.2 ms after the allowance, before its timer.
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::UNAUTHORIZED,
            headers: headers("application/json"),
            body: ScriptedBody::default(),
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the report"
    );
    tokio::time::advance(Duration::from_micros(100)).await;
    let error = match poll_until_idle(invoke.as_mut()) {
        Some(Err(error)) => error,
        Some(Ok(_)) => panic!("a reply must not start from a late error report"),
        None => panic!("a report written after the allowance must fail before the timer fires"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(model.status, None);
    assert_eq!(
        model.reason.as_deref(),
        Some("no response headers arrived within 300 seconds of sending the request")
    );
}

/// A report written exactly at the allowance is on time in either arm: the
/// comparison includes the instant, and the reply starts from it.
#[tokio::test(start_paused = true)]
async fn a_header_report_written_at_the_allowance_stays_on_time() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    // The exchange is polled exactly at the allowance, still before its timer.
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    assert!(
        poll_once(invoke.as_mut()).is_pending(),
        "the exchange must stop after the report"
    );
    tokio::time::advance(Duration::from_micros(100)).await;
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "a report written at the allowance must not be read as late"
    );
    body.push(format!(
        "data: {}\n\n",
        json!({"id": "remote-id", "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": Value::Null}]})
    ));
    let mut stream = poll_until_idle(invoke.as_mut())
        .expect("the report must start the reply")
        .expect("the reply must start after the report");
    let start = next_chunk(&mut stream)
        .expect("the reply must deliver its start")
        .expect("the reply must not fail before its start");
    assert!(matches!(start, ModelChunk::MessageStart { .. }));
    let text = next_chunk(&mut stream)
        .expect("the reply must deliver its text")
        .expect("the first event must deliver text");
    assert!(matches!(text, ModelChunk::Text(text) if text == "hello"));
}

/// The compatible provider returns its stream at the headers, so the exchange
/// completes in the poll that publishes a report written after the allowance:
/// the exchange arm must judge that report before it returns the stream.
#[tokio::test(start_paused = true)]
async fn a_compatible_exchange_completing_after_its_allowance_is_late() {
    let (fetch, reply) = scripted();
    let driver = driver(fetch);
    // Half a tick puts the allowance between two ticks, so the timer fires up
    // to one tick after it.
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiCompatible),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    // The exchange is polled 0.2 ms after the allowance, before its timer.
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: ScriptedBody::default(),
        })
        .unwrap();
    // One poll reports the headers and returns the stream: there is no second
    // exchange poll to read the report in.
    let error = match poll_once(invoke.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a reply must not start from a late report"),
        Poll::Pending => panic!("the exchange completion must be judged in the same poll"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(
        model.reason.as_deref(),
        Some("no response headers arrived within 300 seconds of sending the request")
    );
}

/// A non-2xx reply whose error body is already at EOF reads that body and
/// fails in the poll that publishes its report: while still awaiting headers,
/// the late report ends the wait as missing headers even though the exchange
/// is ready in that same poll.
#[tokio::test(start_paused = true)]
async fn a_late_error_report_with_a_finished_body_fails_as_missing_headers() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let driver = driver_with(Arc::new(EndedBodyFetch {
        reply: Mutex::new(Some(receiver)),
    }));
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    sender
        .send(EndedBodyReply {
            status: http::StatusCode::UNAUTHORIZED,
            headers: headers("application/json"),
        })
        .unwrap();
    let error = match poll_once(invoke.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a reply must not start from a late error report"),
        Poll::Pending => panic!("the completed error body must fail the request in this poll"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, HostErrorKind::Transport);
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(model.status, None);
    assert_eq!(
        model.reason.as_deref(),
        Some("no response headers arrived within 300 seconds of sending the request")
    );
}

/// An exchange failure with no header report keeps its own classification:
/// while the task still awaits headers, a transport failure released after the
/// allowance is not mistaken for missing headers.
#[tokio::test(start_paused = true)]
async fn an_exchange_failure_without_a_report_keeps_its_error() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let driver = driver_with(Arc::new(FailingFetch {
        reply: Mutex::new(Some(receiver)),
    }));
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiChat),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its transport"
    );
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT + Duration::from_micros(200)).await;
    sender
        .send(FetchError::Connect(
            "the connection was refused".to_string().into(),
        ))
        .unwrap();
    let error = match poll_once(invoke.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a failed exchange must not start a stream"),
        Poll::Pending => panic!("a transport failure must end the request without a report"),
    };
    let HostError::Failed {
        kind,
        model: Some(model),
        ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(
        *kind,
        HostErrorKind::Http {
            status: None,
            code: None,
        }
    );
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(model.reason.as_deref(), Some("the connection was refused"));
}

/// A compatible exchange that completes exactly at the allowance is on time:
/// the comparison includes the instant, so the reply still starts and delivers
/// its text.
#[tokio::test(start_paused = true)]
async fn a_compatible_exchange_completing_at_the_allowance_starts() {
    let (fetch, reply) = scripted();
    let body = ScriptedBody::default();
    let driver = driver(fetch);
    tokio::time::advance(Duration::from_micros(500)).await;
    let mut invoke = driver.invoke(
        Arc::new(request()),
        connection(ModelProtocol::OpenaiCompatible),
        CancellationToken::new(),
    );
    assert!(
        poll_until_idle(invoke.as_mut()).is_none(),
        "the request must wait for its response headers"
    );
    // The exchange is polled exactly at the allowance, still before its timer.
    tokio::time::advance(stream::STREAM_IDLE_TIMEOUT).await;
    reply
        .send(ScriptedReply {
            status: http::StatusCode::OK,
            headers: headers("text/event-stream"),
            body: body.clone(),
        })
        .unwrap();
    let mut stream = match poll_once(invoke.as_mut()) {
        Poll::Ready(Ok(stream)) => stream,
        Poll::Ready(Err(error)) => panic!("a report at the allowance must not be late: {error:?}"),
        Poll::Pending => panic!("the exchange completion must be judged in the same poll"),
    };
    body.push(format!(
        "data: {}\n\n",
        json!({"id": "remote-id", "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": Value::Null}]})
    ));
    let start = next_chunk(&mut stream)
        .expect("the reply must deliver its start")
        .expect("the reply must not fail before its start");
    assert!(matches!(start, ModelChunk::MessageStart { .. }));
    let text = next_chunk(&mut stream)
        .expect("the reply must deliver its text")
        .expect("the first event must deliver text");
    assert!(matches!(text, ModelChunk::Text(text) if text == "hello"));
}
