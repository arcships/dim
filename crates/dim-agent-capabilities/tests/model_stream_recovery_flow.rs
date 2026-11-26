use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    defaults,
    host::*,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::{Ctx, Plugin};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// The interval `output.rs` shows thinking progress at.
const THINKING_SHOW_INTERVAL: Duration = Duration::from_millis(250);

fn builder(model: Arc<dyn ModelDriver>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(tools)))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}

/// Hands each attempt the next scripted stream; a request that runs out of
/// scripted streams fails the test.
struct ScriptedModel {
    calls: AtomicUsize,
    streams: Mutex<Vec<mpsc::UnboundedReceiver<std::result::Result<ModelChunk, HostError>>>>,
    second_attempt: Notify,
    release: Notify,
}
impl ScriptedModel {
    fn new(
        streams: Vec<mpsc::UnboundedReceiver<std::result::Result<ModelChunk, HostError>>>,
    ) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            streams: Mutex::new(streams),
            second_attempt: Notify::new(),
            release: Notify::new(),
        }
    }
}
impl ModelDriver for ScriptedModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let receiver = {
                let mut streams = self.streams.lock().unwrap();
                assert!(
                    !streams.is_empty(),
                    "attempt {} has no scripted stream",
                    call + 1
                );
                streams.remove(0)
            };
            if call == 1 {
                self.second_attempt.notify_one();
                self.release.notified().await;
            }
            let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|chunk| (chunk, receiver))
            });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}

fn failure(kind: ModelFailureKind, retry_after: Option<Duration>) -> HostError {
    HostError::Failed {
        message: "the provider rejected the request".into(),
        budget: None,
        kind: HostErrorKind::Transport,
        model: Some(Box::new(ModelFailureFacts {
            kind,
            reason: Some("the provider rejected the request".into()),
            status: None,
            provider_code: None,
            request_id: None,
            cause: Vec::new(),
            retry_after,
            attempts: 1,
            waited: Duration::ZERO,
            retry_after_stop: None,
            retry_forbidden: false,
        })),
        presentation: None,
    }
}

fn malformed_cut() -> HostError {
    HostError::Failed {
        message: "the stream ended without a finish reason".into(),
        budget: None,
        kind: HostErrorKind::Transport,
        model: Some(Box::new(ModelFailureFacts {
            kind: ModelFailureKind::MalformedResponse,
            reason: Some("the stream ended without a finish reason".into()),
            status: None,
            provider_code: None,
            request_id: None,
            cause: Vec::new(),
            retry_after: None,
            attempts: 1,
            waited: Duration::ZERO,
            retry_after_stop: None,
            retry_forbidden: false,
        })),
        presentation: None,
    }
}

fn usage(total: u8) -> ModelChunk {
    ModelChunk::Usage(
        serde_json::from_value(serde_json::json!({
            "promptTokens": 7,
            "completionTokens": total,
            "totalTokens": 7 + total,
        }))
        .unwrap(),
    )
}

fn send(
    chunks: &mpsc::UnboundedSender<std::result::Result<ModelChunk, HostError>>,
    list: Vec<std::result::Result<ModelChunk, HostError>>,
) {
    for chunk in list {
        chunks.send(chunk).unwrap();
    }
}

/// A complete reply, for a stream whose content the test does not inspect.
fn reply(message_id: &str) -> Vec<std::result::Result<ModelChunk, HostError>> {
    vec![
        Ok(ModelChunk::MessageStart {
            message_id: message_id.into(),
        }),
        Ok(ModelChunk::Text("answer".into())),
        Ok(ModelChunk::TextEnd),
        Ok(ModelChunk::MessageStop {
            message_id: message_id.into(),
            reason: ModelStopReason::EndTurn,
        }),
    ]
}

/// Waits until the branch published `count` usage facts, so everything the
/// stream sent before the last one was consumed without moving the clock.
async fn wait_for_usage(session: &Session, count: usize) {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    loop {
        let events = chain_events(session, "main", ..).await.unwrap();
        if events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ModelUsage(_)))
            .count()
            >= count
        {
            return;
        }
        dispatch.changed().await.unwrap();
    }
}

/// Waits until the branch ends its turn; its outcome tells a missing retry
/// apart from a retry still starting.
async fn turn_end(session: &Session) -> String {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    loop {
        let events = chain_events(session, "main", ..).await.unwrap();
        if let Some(outcome) = events.iter().find_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(format!("{:?}", data.outcome)),
            _ => None,
        }) {
            return outcome;
        }
        dispatch.changed().await.unwrap();
    }
}

/// Takes every progress snapshot the cell published since the last call.
fn snapshots(progress: &mut tokio::sync::watch::Receiver<ModelProgress>) -> Vec<ModelProgress> {
    let mut taken = Vec::new();
    while progress.has_changed().unwrap_or(false) {
        taken.push(progress.borrow_and_update().clone());
    }
    taken
}

/// Adds the snapshots a subscriber received since the last call, and their
/// serialized size, to `updates` and `bytes`.
fn drain(
    progress: &mut tokio::sync::watch::Receiver<ModelProgress>,
    updates: &mut usize,
    bytes: &mut usize,
) {
    for snapshot in snapshots(progress) {
        *updates += 1;
        *bytes += serde_json::to_vec(&snapshot).unwrap().len();
    }
}

fn thinking_events(events: &[Arc<AgentEvent>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Thinking(data) => Some(data.delta.clone()),
            _ => None,
        })
        .collect()
}

fn text_events(events: &[Arc<AgentEvent>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Text(data) => Some(data.delta.clone()),
            _ => None,
        })
        .collect()
}

/// The terminal fact's failure, as the wire carries it.
fn model_end_failure(events: &[Arc<AgentEvent>]) -> serde_json::Value {
    let event = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ModelOutputEnd(_)))
        .expect("a model output end");
    serde_json::to_value(event.as_ref()).unwrap()["data"]["failure"].clone()
}

#[tokio::test(start_paused = true)]
async fn a_stream_cut_before_any_committed_event_retries_in_place() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream, retry_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("partial ".into())),
                Ok(ModelChunk::Thinking("thought".into())),
            ],
        );
        progress
            .wait_for(|progress| progress.thinking == "partial thought")
            .await
            .unwrap();
        assert!(
            thinking_events(&chain_events(&session, "main", ..).await.unwrap()).is_empty(),
            "thinking in progress is not a committed fact"
        );
        send(&cut, vec![Err(malformed_cut())]);
        drop(cut);

        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "a cut before any committed output retries in place; the turn ended with {ended:?}"
        );
        let cleared = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            cleared.thinking.is_empty() && cleared.text.is_empty(),
            "the failed attempt's progress is gone before the retry: {cleared:?}"
        );
        assert!(cleared.turn_id.is_some() && cleared.request_id.is_some());

        send(
            &retry,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "retry".into(),
                }),
                Ok(ModelChunk::Thinking("retry thought".into())),
                Ok(ModelChunk::ThinkingEnd {
                    thinking: "retry thought".into(),
                }),
                Ok(ModelChunk::Text("retry answer".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "retry".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(retry);
        model.release.notify_one();
        session.idle("main").await.unwrap();

        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
                .count(),
            1,
            "the retry belongs to the same request"
        );
        assert_eq!(thinking_events(&events), ["retry thought"]);
        assert_eq!(text_events(&events), ["retry answer"]);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        )));
        assert_eq!(
            session.get::<ModelProgress>("main").unwrap().unwrap(),
            ModelProgress::default()
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_stream_cut_after_a_committed_event_ends_the_turn_without_retrying() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let committed = "x".repeat(4096);
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Text(committed.clone())),
                Err(malformed_cut()),
            ],
        );
        drop(cut);
        session.idle("main").await.unwrap();

        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(text_events(&events), [committed]);
        let failure = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    outcome:
                        ModelOutputOutcome::Failed {
                            failure: Some(failure),
                            ..
                        },
                    ..
                }) => Some(failure.as_ref().clone()),
                _ => None,
            })
            .expect("a failed model output with facts");
        assert_eq!(failure.kind, ModelFailureKind::MalformedResponse);
        assert_eq!(failure.attempts, 1, "committed text stops the retry");
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_usage_fact_before_a_cut_stops_the_retry() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "usage".into(),
                }),
                Ok(usage(1)),
                Err(malformed_cut()),
            ],
        );
        drop(cut);
        session.idle("main").await.unwrap();

        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelUsage(_)))
                .count(),
            1,
            "the failed attempt's usage stays the only one"
        );
        let failure = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    outcome:
                        ModelOutputOutcome::Failed {
                            failure: Some(failure),
                            ..
                        },
                    ..
                }) => Some(failure.as_ref().clone()),
                _ => None,
            })
            .expect("a failed model output with facts");
        assert_eq!(
            failure.attempts, 1,
            "a committed usage fact stops the retry"
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// The failure the aimux stream builds when no part arrives inside its idle
/// allowance: a network failure with no provider response behind it.
fn idle_timeout() -> HostError {
    let reason = "the stream delivered nothing for 300 seconds";
    HostError::Failed {
        message: ModelFailureKind::Network.line(Some(reason)),
        budget: None,
        kind: HostErrorKind::Transport,
        model: Some(Box::new(ModelFailureFacts {
            kind: ModelFailureKind::Network,
            reason: Some(reason.into()),
            status: None,
            provider_code: None,
            request_id: None,
            cause: Vec::new(),
            retry_after: None,
            attempts: 1,
            waited: Duration::ZERO,
            retry_after_stop: None,
            retry_forbidden: false,
        })),
        presentation: None,
    }
}

/// An idle timeout before the attempt committed anything is replaced in place,
/// as any network failure is: one request, two attempts, one reply.
#[tokio::test(start_paused = true)]
async fn an_idle_timeout_before_any_committed_event_retries_in_place() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream, retry_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("partial thought".into())),
                Err(idle_timeout()),
            ],
        );
        drop(cut);

        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "an idle timeout before any committed output retries in place; the turn ended with {ended:?}"
        );

        send(&retry, reply("retry"));
        drop(retry);
        model.release.notify_one();
        session.idle("main").await.unwrap();

        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
                .count(),
            1,
            "the retry belongs to the same request"
        );
        assert_eq!(text_events(&events), ["answer"]);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        )));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// An idle timeout after committed text is not replayed: the turn ends with the
/// network failure and resending is the user's call.
#[tokio::test(start_paused = true)]
async fn an_idle_timeout_after_a_committed_event_ends_the_turn_without_retrying() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let committed = "x".repeat(4096);
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Text(committed.clone())),
                Err(idle_timeout()),
            ],
        );
        drop(cut);
        session.idle("main").await.unwrap();

        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(text_events(&events), [committed]);
        let failure = model_end_failure(&events);
        assert_eq!(failure["kind"], "network", "{failure}");
        assert_eq!(failure["attempts"], 1, "committed text stops the retry");
        assert!(matches!(
            events.iter().find_map(|event| match &event.kind {
                EventKind::TurnEnd(data) => Some(&data.outcome),
                _ => None,
            }),
            Some(TurnOutcome::Failed { .. })
        ));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn thinking_progress_is_shown_once_per_interval() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, receiver) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![receiver]));
        let agent = MinimalHost::start(&builder(model, Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "throttle".into(),
                }),
                Ok(ModelChunk::Thinking("a".into())),
                Ok(ModelChunk::Thinking("b".into())),
                Ok(ModelChunk::Thinking("c".into())),
                Ok(usage(1)),
            ],
        );
        wait_for_usage(&session, 1).await;
        let taken = snapshots(&mut progress);
        assert!(
            taken.is_empty(),
            "no snapshot for three deltas inside the interval: {taken:?}"
        );
        let shown = progress
            .wait_for(|progress| progress.thinking == "abc")
            .await
            .unwrap()
            .clone();
        assert_eq!((shown.thinking.as_str(), shown.thinking_offset), ("abc", 0));

        send(
            &chunks,
            vec![
                Ok(ModelChunk::Thinking("d".into())),
                Ok(ModelChunk::Thinking("e".into())),
                Ok(usage(2)),
            ],
        );
        wait_for_usage(&session, 2).await;
        let taken = snapshots(&mut progress);
        assert!(
            taken.is_empty(),
            "no snapshot for the next interval's deltas: {taken:?}"
        );
        let shown = progress
            .wait_for(|progress| progress.thinking == "abcde")
            .await
            .unwrap()
            .clone();
        assert_eq!(shown.thinking.as_str(), "abcde");

        send(
            &chunks,
            vec![
                Ok(ModelChunk::ThinkingEnd {
                    thinking: "abcde".into(),
                }),
                Ok(ModelChunk::Text("answer".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "throttle".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(chunks);
        session.idle("main").await.unwrap();
        assert_eq!(
            session.get::<ModelProgress>("main").unwrap().unwrap(),
            ModelProgress::default()
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            thinking_events(&events),
            ["abcde"],
            "thinking is committed once, at its block end"
        );
        assert_eq!(text_events(&events), ["answer"]);
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_text_delta_shows_buffered_thinking_at_once() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, receiver) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![receiver]));
        let agent = MinimalHost::start(&builder(model, Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "switch".into(),
                }),
                Ok(ModelChunk::Thinking("plan".into())),
                Ok(usage(1)),
            ],
        );
        wait_for_usage(&session, 1).await;
        let taken = snapshots(&mut progress);
        assert!(
            taken.is_empty(),
            "the buffer waits for the interval: {taken:?}"
        );
        send(&chunks, vec![Ok(ModelChunk::Text("answer".into()))]);
        let shown = progress
            .wait_for(|progress| progress.text == "answer")
            .await
            .unwrap()
            .clone();
        assert_eq!(shown.thinking, "plan", "the switch shows the buffer");
        assert_eq!(shown.thinking_offset, 0);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "switch".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(chunks);
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_tool_start_shows_buffered_thinking_at_once() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, receiver) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![receiver]));
        let agent = MinimalHost::start(&builder(model, Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "call".into(),
                }),
                Ok(ModelChunk::Thinking("plan".into())),
                Ok(usage(1)),
            ],
        );
        wait_for_usage(&session, 1).await;
        let taken = snapshots(&mut progress);
        assert!(
            taken.is_empty(),
            "the buffer waits for the interval: {taken:?}"
        );
        send(
            &chunks,
            vec![Ok(ModelChunk::ToolStart {
                id: "call".into(),
                name: "echo".into(),
            })],
        );
        let shown = progress
            .wait_for(|progress| progress.tools.len() == 1)
            .await
            .unwrap()
            .clone();
        assert_eq!(shown.thinking, "plan", "the tool start shows the buffer");
        assert_eq!(shown.thinking_offset, 0);
        drop(chunks);
        session.stop("main", "done".into()).await.unwrap();
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn buffered_thinking_is_shown_within_one_interval_without_another_delta() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, receiver) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![receiver]));
        let agent = MinimalHost::start(&builder(model, Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "alone".into(),
                }),
                Ok(ModelChunk::Thinking("alone".into())),
                Ok(usage(1)),
            ],
        );
        wait_for_usage(&session, 1).await;
        let taken = snapshots(&mut progress);
        assert!(
            taken.is_empty(),
            "nothing is shown before the interval elapses: {taken:?}"
        );
        let shown = progress
            .wait_for(|progress| progress.thinking == "alone")
            .await
            .unwrap()
            .clone();
        assert_eq!(shown.thinking, "alone");

        send(
            &chunks,
            vec![
                Ok(ModelChunk::ThinkingEnd {
                    thinking: "alone".into(),
                }),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "alone".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(chunks);
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_thinking_block_end_shows_the_buffer_at_once() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, receiver) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![receiver]));
        let agent = MinimalHost::start(&builder(model, Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "end".into(),
                }),
                Ok(ModelChunk::Thinking("abc".into())),
                Ok(ModelChunk::ThinkingEnd {
                    thinking: "abc".into(),
                }),
            ],
        );
        let shown = progress
            .wait_for(|progress| progress.thinking_offset + progress.thinking.len() as u64 == 3)
            .await
            .unwrap()
            .clone();
        assert_eq!(
            shown.thinking_offset + shown.thinking.len() as u64,
            3,
            "the block end shows the whole block: {shown:?}"
        );
        tokio::time::advance(THINKING_SHOW_INTERVAL).await;
        tokio::task::yield_now().await;
        let settled = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(
            settled.thinking_offset + settled.thinking.len() as u64,
            3,
            "the interval does not show the block's thinking again: {settled:?}"
        );
        send(
            &chunks,
            vec![
                Ok(ModelChunk::Text("answer".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "end".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(chunks);
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_pending_thinking_buffer_is_not_shown_after_a_retry() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream, retry_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        progress
            .wait_for(|progress| progress.request_id.is_some())
            .await
            .unwrap();
        snapshots(&mut progress);
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("discarded".into())),
                Err(malformed_cut()),
            ],
        );
        drop(cut);

        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "a cut before any committed output retries in place; the turn ended with {ended:?}"
        );
        let cleared = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            cleared.thinking.is_empty(),
            "the failed attempt's buffered thinking is gone before the retry: {cleared:?}"
        );

        model.release.notify_one();
        tokio::time::advance(THINKING_SHOW_INTERVAL).await;
        tokio::task::yield_now().await;
        let retrying = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            retrying.thinking.is_empty(),
            "the retry does not show the failed attempt's buffer: {retrying:?}"
        );

        send(
            &retry,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "retry".into(),
                }),
                Ok(ModelChunk::Thinking("kept".into())),
                Ok(ModelChunk::ThinkingEnd {
                    thinking: "kept".into(),
                }),
                Ok(ModelChunk::Text("answer".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "retry".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ],
        );
        drop(retry);
        session.idle("main").await.unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(thinking_events(&events), ["kept"]);
        assert_eq!(text_events(&events), ["answer"]);
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// Hands each attempt the next scripted stream; the stream that ends in an
/// error reports the drain that follows it and holds it until the test lets
/// it end, so an input can arrive while the failed attempt still drains.
struct DrainingModel {
    calls: AtomicUsize,
    streams: Mutex<Vec<mpsc::UnboundedReceiver<std::result::Result<ModelChunk, HostError>>>>,
    draining: Arc<Notify>,
    release: Arc<Notify>,
}
impl DrainingModel {
    fn new(
        streams: Vec<mpsc::UnboundedReceiver<std::result::Result<ModelChunk, HostError>>>,
    ) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            streams: Mutex::new(streams),
            draining: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}
impl ModelDriver for DrainingModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let receiver = {
                let mut streams = self.streams.lock().unwrap();
                assert!(
                    !streams.is_empty(),
                    "attempt {} has no scripted stream",
                    call + 1
                );
                streams.remove(0)
            };
            let draining = self.draining.clone();
            let release = self.release.clone();
            let stream =
                futures_util::stream::unfold((receiver, false), move |(mut receiver, failed)| {
                    let draining = draining.clone();
                    let release = release.clone();
                    async move {
                        if failed {
                            draining.notify_one();
                            release.notified().await;
                            return None;
                        }
                        receiver.recv().await.map(|item| {
                            let failed = item.is_err();
                            (item, (receiver, failed))
                        })
                    }
                });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}

/// Waits until the applied input's event was dispatched, so every listener,
/// the steer gate included, handled it.
async fn wait_input_applied(session: &Session, input_id: &str) {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    loop {
        let events = chain_events(session, "main", ..).await.unwrap();
        if let Some(sequence) = events.iter().find_map(|event| match &event.kind {
            EventKind::InputApplied(data) if data.input_id == input_id => Some(event.sequence),
            _ => None,
        }) {
            while dispatch.borrow_and_update().sequence < sequence {
                if dispatch.changed().await.is_err() {
                    return;
                }
            }
            return;
        }
        dispatch.changed().await.unwrap();
    }
}

/// A steer consumed while the request waits to retry replaces it: the
/// attempt that showed its thought wrote no fact, so the next attempt starts
/// without visible output and the request is still replaceable.
#[tokio::test(start_paused = true)]
async fn a_steer_during_the_retry_backoff_replaces_the_attempt() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let (steer, steer_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![
            cut_stream,
            retry_stream,
            steer_stream,
        ]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("shown".into())),
            ],
        );
        progress
            .wait_for(|progress| progress.thinking == "shown")
            .await
            .unwrap();
        send(&cut, vec![Err(malformed_cut())]);
        drop(cut);
        progress
            .wait_for(|progress| progress.retry.is_some())
            .await
            .unwrap();
        session
            .submit("STEER-MARKER".into(), InputMode::Steer, "main")
            .await
            .unwrap();
        model.release.notify_one();
        send(&retry, reply("retry"));
        send(&steer, reply("steer"));
        drop(retry);
        drop(steer);
        session.idle("main").await.unwrap();

        let events = chain_events(&session, "main", ..).await.unwrap();
        let calls: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data.request_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            2,
            "the steer starts one request of its own: {calls:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                &event.kind,
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    request_id,
                    outcome: ModelOutputOutcome::Cancelled { reason },
                    ..
                }) if request_id == calls[0]
                    && reason == dim_agent_capabilities::steer::REPLACED_BY_STEER
            )),
            "the waiting attempt was replaced instead of retried: {calls:?}"
        );
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        )));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A steer that arrives while a failed attempt drains does not rewrite a
/// terminal failure: the gate a shown thought closed stays closed, and the
/// turn ends with the provider's failure instead of a replacement.
async fn a_steer_during_a_terminal_failure(kind: ModelFailureKind, retry_after: Option<Duration>) {
    let (cut, cut_stream) = mpsc::unbounded_channel();
    let (steer, steer_stream) = mpsc::unbounded_channel();
    let model = Arc::new(DrainingModel::new(vec![cut_stream, steer_stream]));
    let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
    send(
        &cut,
        vec![
            Ok(ModelChunk::MessageStart {
                message_id: "cut".into(),
            }),
            Ok(ModelChunk::Thinking("shown".into())),
        ],
    );
    progress
        .wait_for(|progress| progress.thinking == "shown")
        .await
        .unwrap();
    send(&cut, vec![Err(failure(kind, retry_after))]);
    model.draining.notified().await;
    let steer_input = session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    wait_input_applied(&session, &steer_input.id).await;
    model.release.notify_one();
    send(&steer, reply("steer"));
    drop(cut);
    drop(steer);
    session.idle("main").await.unwrap();

    let events = chain_events(&session, "main", ..).await.unwrap();
    let first = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request_id.as_str()),
            _ => None,
        })
        .expect("a model call");
    let outcome = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(ModelOutputEndData {
                request_id,
                outcome,
                ..
            }) if request_id == first => Some(outcome),
            _ => None,
        })
        .expect("the first request's terminal");
    assert!(
        matches!(
            outcome,
            ModelOutputOutcome::Failed {
                failure: Some(facts),
                ..
            } if facts.kind == kind
        ),
        "a terminal {kind:?} failure stays a failure: {outcome:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                request_id,
                outcome: ModelOutputOutcome::Cancelled { reason },
                ..
            }) if request_id == first
                && reason == dim_agent_capabilities::steer::REPLACED_BY_STEER
        )),
        "the drain-time steer did not replace the terminal failure"
    );
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Failed { .. },
                ..
            })
        )),
        "the failure ends the turn: {events:?}"
    );
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_steer_during_a_non_retryable_failure_drain_keeps_the_failure() {
    tokio::time::timeout(
        Duration::from_secs(10),
        a_steer_during_a_terminal_failure(ModelFailureKind::InvalidRequest, None),
    )
    .await
    .unwrap();
}

/// A retryable failure whose wait does not fit the budget stops at once; a
/// steer during the drain cannot replace it either.
#[tokio::test(start_paused = true)]
async fn a_steer_during_a_spent_retry_budget_drain_keeps_the_failure() {
    tokio::time::timeout(
        Duration::from_secs(10),
        a_steer_during_a_terminal_failure(
            ModelFailureKind::Network,
            Some(Duration::from_secs(301)),
        ),
    )
    .await
    .unwrap();
}

/// Rejects the first attempt's credential and holds the refresh until the test
/// releases it, while the scripted model keeps the next attempt at the driver.
#[derive(Clone)]
struct CredentialRefresh {
    refreshing: Arc<Notify>,
    released: Arc<Notify>,
}
impl CredentialRefresh {
    fn new() -> Self {
        Self {
            refreshing: Arc::new(Notify::new()),
            released: Arc::new(Notify::new()),
        }
    }
}
impl ResolveModelConnection for CredentialRefresh {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: None,
                recovery: Some(Arc::new(self.clone()) as Arc<dyn RecoverModelConnection>),
            })
        })
    }
}
impl RecoverModelConnection for CredentialRefresh {
    /// A rejected credential is refreshed once; the refreshed connection
    /// rejects it for good.
    fn rejection(&self, error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection {
        if !matches!(
            error,
            HostError::Failed {
                model: Some(facts),
                ..
            } if facts.kind == ModelFailureKind::AuthRejected
        ) {
            return CredentialRejection::Unrelated;
        }
        match phase {
            CredentialRecoveryPhase::Initial => CredentialRejection::Recoverable,
            CredentialRecoveryPhase::Recovered => CredentialRejection::Terminal,
        }
    }
    fn recover(&self, cancel: CancellationToken) -> HostFuture<'_, RecoveredModelConnection> {
        Box::pin(async move {
            self.refreshing.notify_one();
            tokio::select! {
                _ = self.released.notified() => {}
                _ = cancel.cancelled() => {
                    return Err(HostError::Cancelled("the fixture refresh was cancelled".into()));
                }
            }
            Ok(RecoveredModelConnection {
                connection: ModelConnection::Local,
                recovery: Arc::new(self.clone()),
            })
        })
    }
    fn finish_rejected(&self, _: CancellationToken) -> HostFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }
}
struct CredentialRefreshHosts(Arc<ScriptedModel>, CredentialRefresh);
impl Plugin for CredentialRefreshHosts {
    type Deps = ();
    fn name(&self) -> &str {
        "test.credential-refresh"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide(ModelHost(self.0.clone()))?;
        ctx.provide(ProviderConnectionResolver(Arc::new(self.1.clone())))?;
        Ok(())
    }
}

/// A credential refresh keeps the failed attempt's shown thought and does not
/// let a steer replace the request; the retry that follows a successful
/// refresh starts from nothing and is replaceable again.
#[tokio::test(start_paused = true)]
async fn a_credential_refresh_clears_the_attempt_only_once_it_succeeds() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (first, first_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let (steer, steer_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![
            first_stream,
            retry_stream,
            steer_stream,
        ]));
        let refresh = CredentialRefresh::new();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
            .session_value(Arc::new(
                dim_agent_capabilities::history::HistoryRenderer::default(),
            ))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install_with_process_model(&mut builder);
        let driver = model.clone();
        let resolver = refresh.clone();
        builder.plugin(move |_| {
            Ok(Box::new(CredentialRefreshHosts(
                driver.clone(),
                resolver.clone(),
            )))
        });
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        send(
            &first,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "refresh".into(),
                }),
                Ok(ModelChunk::Thinking("shown".into())),
            ],
        );
        progress
            .wait_for(|progress| progress.thinking == "shown")
            .await
            .unwrap();
        let shown = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            shown.thinking_part_id.is_some(),
            "the shown thought has a block identity: {shown:?}"
        );
        send(
            &first,
            vec![Err(failure(ModelFailureKind::AuthRejected, None))],
        );
        drop(first);

        refresh.refreshing.notified().await;
        let refreshing = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(
            refreshing.thinking, "shown",
            "the refresh keeps what the failed attempt showed: {refreshing:?}"
        );
        let during = session
            .submit("STEER-DURING-REFRESH".into(), InputMode::Steer, "main")
            .await
            .unwrap();
        wait_input_applied(&session, &during.id).await;
        let steered = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(
            steered.thinking, "shown",
            "a steer during the refresh drops nothing: {steered:?}"
        );

        refresh.released.notify_one();
        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "a steer during the refresh does not replace the request; the turn ended with {ended:?}"
        );

        let retrying = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            retrying.thinking.is_empty() && retrying.thinking_part_id.is_none(),
            "the retry starts with nothing of the failed attempt shown: {retrying:?}"
        );
        let after = session
            .submit("STEER-AFTER-REFRESH".into(), InputMode::Steer, "main")
            .await
            .unwrap();
        wait_input_applied(&session, &after.id).await;
        drop(retry);
        model.release.notify_one();
        send(&steer, reply("steer"));
        drop(steer);
        session.idle("main").await.unwrap();

        let events = chain_events(&session, "main", ..).await.unwrap();
        let first_call = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data.request_id.as_str()),
                _ => None,
            })
            .expect("a model call");
        let outcome = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    request_id,
                    outcome,
                    ..
                }) if request_id == first_call => Some(outcome),
                _ => None,
            })
            .expect("the first request's terminal");
        assert!(
            matches!(
                outcome,
                ModelOutputOutcome::Cancelled { reason }
                    if reason == dim_agent_capabilities::steer::REPLACED_BY_STEER
            ),
            "the retried attempt is replaced by the steer: {outcome:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                &event.kind,
                EventKind::TurnEnd(TurnEndData {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                })
            )),
            "the steer's own request ends the turn"
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// The comparison the pull request reports: 2,000 deltas of 8 bytes, 20 ms
/// apart, measure the updates and serialized bytes one subscriber sees.
#[tokio::test(start_paused = true)]
async fn thinking_progress_volume_is_throttled() {
    let (chunks, receiver) = mpsc::unbounded_channel();
    let model = Arc::new(ScriptedModel::new(vec![receiver]));
    let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
    progress
        .wait_for(|progress| progress.request_id.is_some())
        .await
        .unwrap();
    let (mut updates, mut bytes) = (0, 0);
    drain(&mut progress, &mut updates, &mut bytes);
    send(
        &chunks,
        vec![Ok(ModelChunk::MessageStart {
            message_id: "volume".into(),
        })],
    );
    tokio::task::yield_now().await;
    for _ in 0..2000 {
        send(&chunks, vec![Ok(ModelChunk::Thinking("abcdefgh".into()))]);
        tokio::time::sleep(Duration::from_millis(20)).await;
        drain(&mut progress, &mut updates, &mut bytes);
    }
    send(
        &chunks,
        vec![
            Ok(ModelChunk::ThinkingEnd {
                thinking: "abcdefgh".repeat(2000),
            }),
            Ok(ModelChunk::Text("answer".into())),
            Ok(ModelChunk::TextEnd),
            Ok(ModelChunk::MessageStop {
                message_id: "volume".into(),
                reason: ModelStopReason::EndTurn,
            }),
        ],
    );
    drop(chunks);
    session.idle("main").await.unwrap();
    drain(&mut progress, &mut updates, &mut bytes);
    println!("thinking progress: {updates} updates, {bytes} bytes");
    assert!(
        updates < 300,
        "the interval throttles 2,000 deltas to {updates} updates"
    );
    assert!(
        bytes < 2_000_000,
        "the throttle keeps the snapshots under 2 MB, got {bytes}"
    );
    agent.close().await.unwrap();
}

/// A retry that replaces a shown attempt and stops without showing anything
/// of its own must say so on the terminal fact: the empty progress frame
/// between the attempts can be coalesced away.
#[tokio::test(start_paused = true)]
async fn a_retry_that_emits_no_output_names_its_boundary_on_the_terminal() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream, retry_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("first attempt".into())),
            ],
        );
        progress
            .wait_for(|progress| progress.thinking == "first attempt")
            .await
            .unwrap();
        assert!(
            thinking_events(&chain_events(&session, "main", ..).await.unwrap()).is_empty(),
            "the shown thought was never committed"
        );
        send(
            &cut,
            vec![Err(failure(ModelFailureKind::ServerError, None))],
        );
        drop(cut);
        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "a cut before any committed output retries in place; the turn ended with {ended:?}"
        );
        model.release.notify_one();
        send(
            &retry,
            vec![Err(failure(ModelFailureKind::InvalidRequest, None))],
        );
        drop(retry);
        session.idle("main").await.unwrap();

        let events = chain_events(&session, "main", ..).await.unwrap();
        let failure = model_end_failure(&events);
        assert_eq!(failure["attempts"], 2, "{failure}");
        assert_eq!(failure["retried"], true, "{failure}");
        assert!(
            failure.get("thinkingPartId").is_none(),
            "the retry showed no thought of its own: {failure}"
        );
        assert!(thinking_events(&events).is_empty());
        assert_eq!(
            session.get::<ModelProgress>("main").unwrap().unwrap(),
            ModelProgress::default()
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// The final attempt's own shown thought is not the replaced attempt's: the
/// terminal fact names the block it left open, so clients keep that row.
#[tokio::test(start_paused = true)]
async fn a_retry_that_shows_its_own_thought_names_that_block_on_the_terminal() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cut, cut_stream) = mpsc::unbounded_channel();
        let (retry, retry_stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![cut_stream, retry_stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        send(
            &cut,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "cut".into(),
                }),
                Ok(ModelChunk::Thinking("first attempt".into())),
            ],
        );
        progress
            .wait_for(|progress| progress.thinking == "first attempt")
            .await
            .unwrap();
        send(
            &cut,
            vec![Err(failure(ModelFailureKind::ServerError, None))],
        );
        drop(cut);
        let ended = tokio::select! {
            _ = model.second_attempt.notified() => None,
            outcome = turn_end(&session) => Some(outcome),
        };
        assert!(
            ended.is_none(),
            "the first attempt retries in place; the turn ended with {ended:?}"
        );
        model.release.notify_one();
        send(
            &retry,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "retry".into(),
                }),
                Ok(ModelChunk::Thinking("second attempt".into())),
            ],
        );
        // The guard holds the progress cell; drop it before the request can
        // clear the progress and finish the turn.
        let part = {
            let shown = progress
                .wait_for(|progress| progress.thinking == "second attempt")
                .await
                .unwrap();
            shown
                .thinking_part_id
                .clone()
                .expect("the shown thought names its block")
        };
        send(
            &retry,
            vec![Err(failure(ModelFailureKind::InvalidRequest, None))],
        );
        drop(retry);
        session.idle("main").await.unwrap();

        let events = chain_events(&session, "main", ..).await.unwrap();
        let failure = model_end_failure(&events);
        assert_eq!(failure["attempts"], 2, "{failure}");
        assert_eq!(failure["retried"], true, "{failure}");
        assert_eq!(failure["thinkingPartId"], part, "{failure}");
        assert!(thinking_events(&events).is_empty());
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A request that was never retried omits the retry boundary.
#[tokio::test(start_paused = true)]
async fn a_failure_without_a_retry_omits_the_retry_boundary() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (chunks, stream) = mpsc::unbounded_channel();
        let model = Arc::new(ScriptedModel::new(vec![stream]));
        let agent = MinimalHost::start(&builder(model.clone(), Arc::new(MockTools)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default(), Default::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        send(
            &chunks,
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "only".into(),
                }),
                Ok(ModelChunk::Thinking("only attempt".into())),
                Err(failure(ModelFailureKind::InvalidRequest, None)),
            ],
        );
        drop(chunks);
        session.idle("main").await.unwrap();

        let events = chain_events(&session, "main", ..).await.unwrap();
        let failure = model_end_failure(&events);
        assert_eq!(failure["attempts"], 1, "{failure}");
        assert!(failure.get("retried").is_none(), "{failure}");
        assert!(failure.get("thinkingPartId").is_none(), "{failure}");
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}
