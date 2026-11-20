//! The progress reminder: one unkeyed Append input after a silent stretch of
//! tool calls, once per stretch, interactive Sessions only.
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    defaults,
    history::HistoryRenderer,
    host::*,
    progress_reminder::{ProgressReminderConfig, REMINDER},
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

/// One model reply the scripted model may make.
enum Step {
    Text(&'static str),
    Tool(&'static str, serde_json::Value),
    TextThenTool(&'static str, &'static str, serde_json::Value),
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
                Some(Step::Tool(name, input)) => {
                    chunks.extend(tool_chunks(name, input));
                    ModelStopReason::ToolUse
                }
                Some(Step::TextThenTool(text, name, input)) => {
                    chunks.push(Ok(ModelChunk::Text(text.into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    chunks.extend(tool_chunks(name, input));
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
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}
fn tool_chunks(
    name: &str,
    input: serde_json::Value,
) -> Vec<std::result::Result<ModelChunk, HostError>> {
    let call = ToolCall {
        id: format!("call-{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
        name: name.into(),
        input: input.as_object().unwrap().clone(),
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
        defaults::install_with_progress_reminder(&mut builder, ProgressReminderConfig { after });
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
async fn idle(session: &Session) {
    session.idle("main").await.unwrap();
}

/// Every reminder input of `main`, oldest first.
async fn reminders(session: &Session) -> Vec<InputMessageData> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if data.blocks.iter().any(|block| {
                    let InputBlock::Raw { text, .. } = block;
                    text.starts_with(&format!("<{REMINDER}"))
                }) =>
            {
                Some(data.clone())
            }
            _ => None,
        })
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
fn said(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
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
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(start_paused = true)]
async fn one_reminder_after_the_threshold_arrives_as_an_append() {
    let model = ScriptedModel::new(vec![
        Step::TextThenTool("starting", "gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(9).await;
    assert!(
        reminders(&session).await.is_empty(),
        "no reminder before the threshold"
    );

    pass(2).await;
    let sent = reminders(&session).await;
    assert_eq!(sent.len(), 1, "one reminder after the threshold");
    assert_eq!(
        sent[0].mode,
        Some(InputMode::Append),
        "the reminder is an Append"
    );
    let InputBlock::Raw { text, key, .. } = &sent[0].blocks[0];
    let seconds = text
        .strip_prefix("<dim-progress-reminder seconds=\"")
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("the block states its seconds");
    assert!(
        seconds >= 10,
        "the block states the elapsed stretch: {text}"
    );
    assert_eq!(*key, None, "the reminder is unkeyed");

    env.gate.release.notify_waiters();
    idle(&session).await;
    let sent = requests(&session).await;
    assert!(sent.len() >= 2, "the tool result earns another request");
    assert!(
        said(&sent[1]).contains("<dim-progress-reminder"),
        "{}",
        said(&sent[1])
    );
    assert_eq!(
        sent[0].system, sent[1].system,
        "the reminder does not move the cached prefix"
    );
    let before = &sent[0].messages;
    let after = &sent[1].messages;
    assert!(
        after.starts_with(before),
        "the earlier messages stay as they were"
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn text_within_the_window_earns_no_reminder() {
    let model = ScriptedModel::new(vec![
        Step::TextThenTool("already talking", "gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(9).await;
    assert!(
        reminders(&session).await.is_empty(),
        "the text opened the window"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    assert!(
        reminders(&session).await.is_empty(),
        "no reminder after the text"
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_later_text_resets_the_stretch() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate", serde_json::json!({})),
        Step::TextThenTool("progress", "gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(5).await;
    env.gate.release.notify_waiters();
    while env.gate.entered.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    pass(9).await;
    assert!(
        reminders(&session).await.is_empty(),
        "the later text moved the deadline"
    );
    pass(2).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "the reset window earns one"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_long_tool_call_earns_one_reminder() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(31).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "three thresholds of one tool call earn one reminder"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    let sent = requests(&session).await;
    assert!(sent.len() >= 2, "the tool result earns another request");
    assert_eq!(
        said(&sent[1]).matches("<dim-progress-reminder").count(),
        1,
        "{}",
        said(&sent[1])
    );
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_new_tool_call_earns_the_next_reminder() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate", serde_json::json!({})),
        Step::Tool("gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert_eq!(reminders(&session).await.len(), 1, "the first stretch");

    env.gate.release.notify_waiters();
    while env.gate.entered.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    pass(8).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "the next stretch counts from the reminder"
    );
    pass(2).await;
    assert_eq!(
        reminders(&session).await.len(),
        2,
        "the new tool call earns the second"
    );
    pass(10).await;
    assert_eq!(
        reminders(&session).await.len(),
        2,
        "the second call earns no third"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_non_interactive_session_gets_none() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
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
    env.host.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_reload_writes_no_second_reminder() {
    let model = ScriptedModel::new(vec![
        Step::Tool("gate", serde_json::json!({})),
        Step::Text("done"),
    ]);
    let env = Env::start(model, Duration::from_secs(10)).await;
    let session = env.session(true).await;
    submit(&session).await;
    reached(&env.gate).await;

    pass(11).await;
    assert_eq!(reminders(&session).await.len(), 1, "the first window");

    let id = env
        .host
        .root()
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == "ProgressReminder")
        .expect("the part is mounted")
        .id;
    env.host
        .root()
        .view(id)
        .expect("the part's fiber")
        .restart()
        .await
        .unwrap();
    pass(30).await;
    assert_eq!(
        reminders(&session).await.len(),
        1,
        "the rebuilt stretch needs a tool call after the reminder"
    );

    env.gate.release.notify_waiters();
    idle(&session).await;
    env.host.close().await.unwrap();
}
