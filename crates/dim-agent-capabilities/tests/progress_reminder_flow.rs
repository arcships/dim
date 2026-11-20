//! The progress reminder: the Loop adds one input to the request it starts
//! after a silent stretch of tool calls, so that request consumes it at once
//! and no later turn can see it. Interactive Sessions only.
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    agent_loop::{LoopConfig, REMINDER},
    defaults,
    history::HistoryRenderer,
    host::*,
    input_queue::InputQueue,
    tool_registry::definition,
};
use dim_agent_runtime::testing::{MinimalHost, chain_events};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::CtxHost;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// A block a tool call or a model reply waits behind.
#[derive(Default)]
struct Gate {
    entered: AtomicUsize,
    release: Notify,
}

/// One model reply the scripted model may make.
enum Step {
    Text(&'static str),
    Tool(&'static str),
    TextThenTool(&'static str, &'static str),
    /// The request is in flight while the test holds this gate, then the
    /// reply is text that ends the turn without a tool call.
    Waiting(Arc<Gate>, &'static str),
}

struct ScriptedModel(Mutex<VecDeque<Step>>);
impl ScriptedModel {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(steps.into())))
    }
}
impl ModelDriver for ScriptedModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let step = self.0.lock().unwrap().pop_front();
        Box::pin(async move {
            let id = "message".to_owned();
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match step {
                Some(Step::Tool(name)) => {
                    chunks.extend(tool_chunks(name));
                    ModelStopReason::ToolUse
                }
                Some(Step::TextThenTool(text, name)) => {
                    chunks.push(Ok(ModelChunk::Text(text.into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    chunks.extend(tool_chunks(name));
                    ModelStopReason::ToolUse
                }
                Some(Step::Waiting(gate, text)) => {
                    gate.entered.fetch_add(1, Ordering::SeqCst);
                    gate.release.notified().await;
                    chunks.push(Ok(ModelChunk::Text(text.into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
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
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}
fn tool_chunks(name: &str) -> Vec<std::result::Result<ModelChunk, HostError>> {
    let call = ToolCall {
        id: format!("call-{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
        name: name.into(),
        input: serde_json::Map::new(),
    };
    vec![
        Ok(ModelChunk::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
        }),
        Ok(ModelChunk::ToolUse(call)),
    ]
}
static COUNTER: AtomicUsize = AtomicUsize::new(0);

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
            Ok(ToolResult::text("gated"))
        })
    }
}

struct Env {
    host: MinimalHost,
    gate: Arc<Gate>,
}
impl Env {
    async fn start(model: Arc<ScriptedModel>, after: Duration) -> Self {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        let gate = Arc::new(Gate::default());
        builder
            .process_value(Arc::new(ModelHost(model)))
            .session_value(Arc::new(ToolHost(Arc::new(GateTool(gate.clone())))))
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
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
        Self { host, gate }
    }
    async fn session(&self, interactive: bool) -> Session {
        self.host
            .create_session(SessionConfig {
                tools: Some(vec!["gate".into()]),
                interactive,
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }
}

/// Paused time moves through this sleep: it auto-advances to the next timer,
/// so no test waits in real time.
async fn pass(seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}
async fn reached(gate: &Gate) {
    while gate.entered.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
}
async fn submit(session: &Session) {
    session
        .submit("work".into(), InputMode::Queue, "main")
        .await
        .unwrap();
}
async fn submit_again(session: &Session) {
    session
        .submit("again".into(), InputMode::Queue, "main")
        .await
        .unwrap();
}
async fn idle(session: &Session) {
    session.idle("main").await.unwrap();
}

/// Whether `data` is a progress reminder input.
fn is_reminder(data: &InputMessageData) -> bool {
    data.blocks.iter().any(|block| {
        let InputBlock::Raw { text, .. } = block;
        text.starts_with(&format!("<{REMINDER}"))
    })
}

/// Every reminder input of `main`, oldest first.
async fn reminders(session: &Session) -> Vec<InputMessageData> {
    reminder_events(session)
        .await
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.clone()),
            _ => None,
        })
        .collect()
}

/// Every reminder event of `main`, oldest first.
async fn reminder_events(session: &Session) -> Vec<Arc<AgentEvent>> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if is_reminder(data)))
        .collect()
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
fn message_text(message: &Message) -> String {
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
fn said(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| message_text(message))
        .collect::<Vec<_>>()
        .join("\n")
}
fn last_message(request: &ModelRequest) -> String {
    request
        .messages
        .last()
        .map(|message| message_text(message))
        .unwrap_or_default()
}
fn reminder_count(request: &ModelRequest) -> usize {
    said(request).matches("<dim-progress-reminder").count()
}

#[tokio::test(start_paused = true)]
async fn a_silent_tool_call_earns_one_reminder_in_the_next_request() {
    let model = ScriptedModel::new(vec![Step::Tool("gate"), Step::Text("done")]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert!(
        reminders(&session).await.is_empty(),
        "nothing is written while only a tool call is in flight"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    let sent = requests(&session).await;
    assert_eq!(sent.len(), 2, "the tool result earns the next request");
    assert_eq!(reminder_count(&sent[0]), 0, "{}", said(&sent[0]));
    assert_eq!(reminder_count(&sent[1]), 1, "{}", said(&sent[1]));
    assert!(
        last_message(&sent[1]).contains("<dim-progress-reminder"),
        "the reminder is the request's last model message: {}",
        last_message(&sent[1])
    );
    assert_eq!(
        sent[0].system, sent[1].system,
        "the reminder does not move the cached prefix"
    );
    assert!(
        sent[1].messages.starts_with(&sent[0].messages),
        "the earlier messages stay as they were"
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    let reminder = reminder_events(&session).await;
    assert_eq!(reminder.len(), 1, "one reminder");
    let reminder_id = reminder[0].id.clone();
    let consumed = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == reminder_id)
        })
        .expect("the request consumes the reminder");
    let turn = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::TurnStart(data) => Some(data.turn_id.clone()),
            _ => None,
        })
        .expect("a turn");
    let ran = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelStart(data) => Some(data.request_id.clone()),
            _ => None,
        })
        .expect("the request that ran the tool call");
    let EventKind::InputMessage(data) = &reminder[0].kind else {
        unreachable!("a reminder is an input")
    };
    assert_eq!(
        data.handover,
        Some(InputHandover {
            turn_id: turn,
            request_id: ran,
        }),
        "the reminder is addressed to the continuation of the request whose tool call ran"
    );
    let carrier = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data)
                if said(&data.request).contains("<dim-progress-reminder") =>
            {
                Some(data.request_id.clone())
            }
            _ => None,
        })
        .expect("the request that carries the reminder");
    let start = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::ModelStart(data) if data.request_id == carrier))
        .expect("the reminder's request starts");
    assert!(
        reminder[0].sequence < consumed.sequence && consumed.sequence < start.sequence,
        "the reminder is written, consumed, and only then does its request start: {} < {} < {}",
        reminder[0].sequence,
        consumed.sequence,
        start.sequence
    );
    assert!(
        !events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == reminder_id)),
        "the queue never applies a handover reminder"
    );

    submit_again(&session).await;
    idle(&session).await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        !events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == reminder_id)),
        "no applied fact appears for the reminder after the next turn starts"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
                EventKind::InputConsumed(data) if data.input_id == reminder_id))
            .count(),
        1,
        "the reminder is consumed once"
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn text_within_the_window_earns_no_reminder() {
    let model = ScriptedModel::new(vec![
        Step::TextThenTool("already talking", "gate"),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(9).await;
    env.gate.release.notify_waiters();
    idle(&session).await;
    assert!(
        reminders(&session).await.is_empty(),
        "the text opened the window"
    );
    let sent = requests(&session).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(reminder_count(&sent[1]), 0, "{}", said(&sent[1]));
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_new_tool_call_earns_the_next_reminder() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate"),
        Step::Tool("gate"),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert!(
        reminders(&session).await.is_empty(),
        "the first reminder waits for a request"
    );
    env.gate.release.notify_waiters();
    while env.gate.entered.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    pass(9).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "the next stretch counts from the reminder"
    );
    pass(2).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "the second reminder waits for the request it belongs to"
    );
    env.gate.release.notify_waiters();
    idle(&session).await;

    let sent = requests(&session).await;
    assert_eq!(sent.len(), 3, "two tool results earn three requests");
    assert_eq!(reminder_count(&sent[1]), 1, "{}", said(&sent[1]));
    assert_eq!(
        reminder_count(&sent[2]),
        2,
        "the second reminder joins the first in the history: {}",
        said(&sent[2])
    );
    assert_eq!(
        reminders(&session).await.len(),
        2,
        "one reminder per silent stretch"
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_silent_final_request_does_not_reach_the_next_turn() {
    let waiting = Arc::new(Gate::default());
    let model = ScriptedModel::new(vec![
        Step::Tool("gate"),
        Step::Waiting(waiting.clone(), "final"),
        Step::Text("next"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    env.gate.release.notify_waiters();
    reached(&waiting).await;
    pass(11).await;
    assert!(
        reminders(&session).await.is_empty(),
        "the request in flight writes nothing"
    );
    waiting.release.notify_waiters();
    idle(&session).await;

    submit_again(&session).await;
    idle(&session).await;
    let sent = requests(&session).await;
    assert_eq!(sent.len(), 3, "one request per model step");
    assert_eq!(
        reminder_count(sent.last().expect("a last request")),
        0,
        "the next turn starts a new stretch: {}",
        said(sent.last().expect("a last request"))
    );
    assert!(reminders(&session).await.is_empty());
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_ends_the_stretch() {
    let model = ScriptedModel::new(vec![Step::Tool("gate"), Step::Text("done")]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert!(reminders(&session).await.is_empty());
    session.interrupt("main", "stop".into()).unwrap();
    env.gate.release.notify_waiters();
    idle(&session).await;

    submit_again(&session).await;
    idle(&session).await;
    let sent = requests(&session).await;
    assert_eq!(sent.len(), 2, "the interrupted turn made one request");
    assert_eq!(
        reminder_count(sent.last().expect("a last request")),
        0,
        "the next turn's first request starts a new stretch: {}",
        said(sent.last().expect("a last request"))
    );
    assert!(reminders(&session).await.is_empty());
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_non_interactive_session_gets_none() {
    let model = ScriptedModel::new(vec![Step::Tool("gate"), Step::Text("done")]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(false).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(300).await;
    assert!(
        reminders(&session).await.is_empty(),
        "a non-interactive Session"
    );
    env.gate.release.notify_waiters();
    idle(&session).await;
    assert!(reminders(&session).await.is_empty());
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_reload_does_not_decide_again() {
    let model = ScriptedModel::new(vec![Step::Tool("gate"), Step::Text("done")]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert!(reminders(&session).await.is_empty());
    let id = env
        .host
        .root()
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == "Loop")
        .expect("the part is mounted")
        .id;
    env.host
        .root()
        .view(id)
        .expect("the part's fiber")
        .restart()
        .await
        .unwrap();
    env.gate.release.notify_waiters();
    idle(&session).await;
    assert!(
        reminders(&session).await.is_empty(),
        "restore never re-decides a request that already started"
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_reload_after_the_reminders_turn_keeps_only_its_consumption() {
    let model = ScriptedModel::new(vec![Step::Tool("gate"), Step::Text("done")]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    env.gate.release.notify_waiters();
    idle(&session).await;
    let reminder = reminder_events(&session).await.pop().expect("one reminder");
    let id = reminder.id.clone();
    let loop_id = env
        .host
        .root()
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == "Loop")
        .expect("the part is mounted")
        .id;
    env.host
        .root()
        .view(loop_id)
        .expect("the part's fiber")
        .restart()
        .await
        .unwrap();

    let queue = session.service::<InputQueue>("main").unwrap();
    assert!(
        !queue.pending(&id),
        "the consumed reminder is not pending after reload"
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        !events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == id)),
        "reload reveals no applied fact for the reminder"
    );
    assert!(
        !events.iter().any(|event| matches!(&event.kind,
            EventKind::InputDeleted(data) if data.input_id == id)),
        "reload writes no deletion for the reminder"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
                EventKind::InputConsumed(data) if data.input_id == id))
            .count(),
        1,
        "the reminder is consumed once, not again after reload"
    );
    env.host.close().await.unwrap();
}
