//! A request-scoped input belongs to the request whose batch consumed it
//! (ContextCompaction §5.9): compaction neither pins nor summarises it, and
//! the request being prepared keeps its own reminder as its last message.
use super::*;
use dim_agent_capabilities::agent_loop::LoopConfig;
use dim_agent_capabilities::tool_registry::definition;
use dim_agent_runtime::testing::chain_events;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::Notify;

const MARKER: &str = "<dim-progress-reminder";
const RESULT: usize = 1_200;

/// A tool call the test holds until it releases the gate.
#[derive(Default)]
struct Gate {
    entered: AtomicUsize,
    release: Notify,
}

struct GateTool(Arc<Gate>);
impl ToolDriver for GateTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..definition(
                "gate",
                "Block until the test releases it.",
                serde_json::json!({"type":"object"}),
            )
        }]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.0.entered.fetch_add(1, Ordering::SeqCst);
            self.0.release.notified().await;
            Ok(ToolResult::text("r".repeat(RESULT)))
        })
    }
}

enum Step {
    Tool,
    Text(&'static str),
}

struct Scripted(Mutex<VecDeque<Step>>);
impl ModelDriver for Scripted {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let step = self.0.lock().unwrap().pop_front();
        Box::pin(async move {
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: "m".into(),
            })];
            let reason = match step {
                Some(Step::Tool) => {
                    chunks.push(Ok(ModelChunk::ToolStart {
                        id: "gate-call".into(),
                        name: "gate".into(),
                    }));
                    chunks.push(Ok(ModelChunk::ToolUse(ToolCall {
                        id: "gate-call".into(),
                        name: "gate".into(),
                        input: serde_json::Map::new(),
                    })));
                    ModelStopReason::ToolUse
                }
                Some(Step::Text(text)) => {
                    chunks.push(Ok(ModelChunk::Text(text.into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
                }
                None => {
                    chunks.push(Ok(ModelChunk::Text("done".into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
                }
            };
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: "m".into(),
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

/// Window facts whose threshold the test moves: infinite never compacts, so a
/// request can be prepared first and compaction made to run on a later one.
struct Window(Arc<AtomicU64>);
impl ResolveContextModel for Window {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        let threshold = self.0.clone();
        Box::pin(async move { Ok(Arc::new(Window(threshold)) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for Window {
    fn context_window(&self) -> f64 {
        4096.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::SeqCst))
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
    fn contribution_counter(&self) -> Option<&dyn ContextCounter> {
        Some(self)
    }
}
impl ContextCounter for Window {
    fn algorithm(&self) -> &str {
        "test/message-array/v1"
    }
    fn fixed(&self, _: Option<&SystemPrompt>, _: Option<&[ToolDefinition]>) -> ContextContribution {
        ContextContribution {
            serialized_bytes: 1,
            images: 0,
        }
    }
    fn message(&self, message: &Message) -> ContextContribution {
        ContextContribution {
            serialized_bytes: serde_json::to_vec(message).unwrap().len() as u64 + 1,
            images: 0,
        }
    }
    fn estimate(&self, contribution: ContextContribution) -> f64 {
        contribution.serialized_bytes.max(2) as f64
    }
}

/// What the strategy was handed, so a test can look for a marker in it.
#[derive(Default)]
struct Seen {
    history: Mutex<Vec<String>>,
    request: Mutex<Vec<String>>,
}

struct Recorder(Arc<Seen>);
impl CompactionStrategy for Recorder {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new("", "", vec![summary_message("short")]))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        *self.0.history.lock().unwrap() = input
            .history
            .iter()
            .map(|entry| text_of(&entry.message))
            .collect();
        *self.0.request.lock().unwrap() = input
            .request
            .messages
            .iter()
            .map(|message| text_of(message))
            .collect();
        Box::pin(async move {
            let Some(cut) = input.cut() else {
                return Ok(StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: test_diagnostics(),
                });
            };
            Ok(StrategyOutcome::Completed {
                retain_from: cut.retain_from.clone(),
                replacement: vec![summary_message("short")],
                diagnostics: test_diagnostics(),
            })
        })
    }
}

struct Env {
    host: MinimalHost,
    gate: Arc<Gate>,
    seen: Arc<Seen>,
    threshold: Arc<AtomicU64>,
}
impl Env {
    async fn start(steps: Vec<Step>, after: Duration) -> Self {
        let gate = Arc::new(Gate::default());
        let seen = Arc::new(Seen::default());
        let threshold = Arc::new(AtomicU64::new(f64::INFINITY.to_bits()));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .timeline_store(TimelineStoreBinding::new(Arc::new(
                MemoryTimelineStore::default(),
            )))
            .process_value(Arc::new(ModelHost(Arc::new(Scripted(Mutex::new(
                steps.into(),
            ))))))
            .session_value(Arc::new(ToolHost(Arc::new(GateTool(gate.clone())))))
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(Some(Arc::new(Window(
                threshold.clone(),
            ))))))
            .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Recorder(
                seen.clone(),
            ))))))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install_with_loop_config(
            &mut builder,
            LoopConfig {
                reminder_after: after,
            },
        );
        let host = MinimalHost::start(&builder).await.unwrap();
        Self {
            host,
            gate,
            seen,
            threshold,
        }
    }
    async fn session(&self) -> Session {
        self.host
            .create_session(SessionConfig {
                tools: Some(vec!["gate".into()]),
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }
    fn compacting(&self) {
        self.threshold.store(1f64.to_bits(), Ordering::SeqCst);
    }
}

async fn reach(gate: &Gate, nth: usize) {
    while gate.entered.load(Ordering::SeqCst) < nth {
        tokio::task::yield_now().await;
    }
}
async fn pass(seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}
async fn submit(session: &Session, text: &str, mode: InputMode) {
    session.submit(text.into(), mode, "main").await.unwrap();
}
async fn requests(session: &Session) -> Vec<Arc<ModelRequest>> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .collect()
}
fn text_of(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.clone()),
                MessagePart::ToolResult { content, .. } => match content {
                    ToolContent::Text(text) => Some(text.clone()),
                    ToolContent::Blocks(_) => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
fn markers<'a>(messages: impl IntoIterator<Item = &'a Message>) -> usize {
    messages
        .into_iter()
        .map(|message| text_of(message).matches(MARKER).count())
        .sum()
}
/// The completed compaction's data and the index of its event.
fn compaction(events: &[Arc<AgentEvent>]) -> (usize, ContextCompactionData) {
    events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. }))
        })
        .map(|index| {
            let EventKind::ContextCompaction(data) = &events[index].kind else {
                unreachable!("matched a compaction")
            };
            (index, data.clone())
        })
        .expect("no completed compaction")
}
/// The event id of the reminder's `input/message`.
fn reminder_input(events: &[Arc<AgentEvent>]) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if data
                    .blocks
                    .iter()
                    .any(|block| matches!(block, InputBlock::Raw { text, .. } if text.starts_with(MARKER))) =>
            {
                Some(event.id.clone())
            }
            _ => None,
        })
        .expect("the reminder input")
}
fn completed(data: &ContextCompactionData) -> (&Vec<Message>, Option<&[String]>) {
    let ContextCompactionResult::Completed {
        replacement,
        pinned_inputs,
        ..
    } = &data.outcome
    else {
        unreachable!("a completed compaction")
    };
    (replacement, pinned_inputs.as_deref())
}
fn request_after(events: &[Arc<AgentEvent>], at: usize) -> Arc<ModelRequest> {
    events[at..]
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("the request after the compaction")
}

/// A cut after a consumed reminder pins the turn's ordinary inputs but not
/// the reminder, and the next request carries none (ContextCompaction §5.9).
#[tokio::test(start_paused = true)]
async fn a_cut_after_the_reminder_neither_pins_nor_summarises_it() {
    let env = Env::start(
        vec![Step::Tool, Step::Tool, Step::Text("done")],
        Duration::from_secs(10),
    )
    .await;
    let session = env.session().await;
    submit(&session, "work", InputMode::Queue).await;
    reach(&env.gate, 1).await;
    submit(&session, "note", InputMode::Append).await;
    pass(11).await;
    env.gate.release.notify_one();
    reach(&env.gate, 2).await;

    let sent = requests(&session).await;
    assert_eq!(sent.len(), 2, "two requests before the compaction");
    assert_eq!(
        markers(sent[1].messages.iter().map(Arc::as_ref)),
        1,
        "the continuation consumes the reminder: {:?}",
        sent[1].messages
    );
    assert!(
        text_of(sent[1].messages.last().unwrap()).starts_with(MARKER),
        "the reminder is that request's last message"
    );

    env.compacting();
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();

    let events = chain_events(&session, "main", ..).await.unwrap();
    let (at, data) = compaction(&events);
    let (replacement, pinned) = completed(&data);
    assert_eq!(
        markers(replacement),
        0,
        "the persisted replacement pins no reminder: {replacement:?}"
    );
    assert!(
        replacement.iter().any(|message| text_of(message) == "note"),
        "an ordinary input consumed in the same span is still pinned: {replacement:?}"
    );
    assert!(
        replacement.iter().any(|message| text_of(message) == "work"),
        "the turn's own input is still pinned: {replacement:?}"
    );
    assert!(
        !pinned
            .unwrap_or_default()
            .contains(&reminder_input(&events)),
        "the pins do not name the reminder"
    );
    let after = request_after(&events, at);
    assert_eq!(
        markers(after.messages.iter().map(Arc::as_ref)),
        0,
        "the next request carries no reminder: {:?}",
        after.messages
    );
    env.host.close().await.unwrap();
}

/// A compaction in the reminder's own request drops it from the strategy's
/// input and from the record, and the request still ends with it.
#[tokio::test(start_paused = true)]
async fn a_compaction_in_the_reminders_own_request_keeps_only_its_tail() {
    let env = Env::start(
        vec![Step::Tool, Step::Text("done")],
        Duration::from_secs(10),
    )
    .await;
    let session = env.session().await;
    submit(&session, "work", InputMode::Queue).await;
    reach(&env.gate, 1).await;
    pass(11).await;
    env.compacting();
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();

    let events = chain_events(&session, "main", ..).await.unwrap();
    let (at, data) = compaction(&events);
    let (replacement, pinned) = completed(&data);
    assert_eq!(
        markers(replacement),
        0,
        "the record contains no reminder: {replacement:?}"
    );
    assert!(
        !pinned
            .unwrap_or_default()
            .contains(&reminder_input(&events)),
        "the pins do not name the reminder"
    );
    let history = env.seen.history.lock().unwrap().clone();
    assert!(
        history.iter().all(|text| !text.contains(MARKER)),
        "the strategy's history has no reminder: {history:?}"
    );
    let request = env.seen.request.lock().unwrap().clone();
    assert!(
        request.iter().all(|text| !text.contains(MARKER)),
        "the request it is handed has no reminder: {request:?}"
    );
    let after = request_after(&events, at);
    assert_eq!(
        markers(after.messages.iter().map(Arc::as_ref)),
        1,
        "the request being prepared keeps its reminder: {:?}",
        after.messages
    );
    assert!(
        text_of(after.messages.last().unwrap()).starts_with(MARKER),
        "as its last message"
    );
    assert_eq!(
        after.messages[..replacement.len()]
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>(),
        replacement.iter().collect::<Vec<_>>(),
        "the provider request differs from the record only by the reminder"
    );
    assert_eq!(
        after.messages.len(),
        replacement.len() + 1,
        "and by nothing else"
    );
    env.host.close().await.unwrap();
}

/// The same cut after a restore: the persisted pins carry no reminder into
/// the next turn's request.
#[tokio::test(start_paused = true)]
async fn a_restored_session_after_the_cut_carries_no_reminder() {
    let env = Env::start(
        vec![Step::Tool, Step::Tool, Step::Text("done")],
        Duration::from_secs(10),
    )
    .await;
    let session = env.session().await;
    submit(&session, "work", InputMode::Queue).await;
    reach(&env.gate, 1).await;
    pass(11).await;
    env.gate.release.notify_one();
    reach(&env.gate, 2).await;
    env.compacting();
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();

    let id = session.id().to_owned();
    session.close().await.unwrap();
    let restored = env.host.agent().restore_session(id).await.unwrap();
    submit(&restored, "again", InputMode::Queue).await;
    restored.idle("main").await.unwrap();
    let sent = requests(&restored).await;
    let next = sent.last().expect("the next turn's request");
    assert_eq!(
        markers(next.messages.iter().map(Arc::as_ref)),
        0,
        "a restored Session's request carries no reminder: {:?}",
        next.messages
    );
    env.host.close().await.unwrap();
}
