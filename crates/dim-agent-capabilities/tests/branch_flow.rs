use crate::support;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::HistoryRenderer,
    host::{
        CompactionInput, CompactionStrategy, CompactionStrategyHost, ContextModel,
        ContextModelHost, HostError, HostFuture, IdentityContext, ModelChunk, ModelConnection,
        ModelDriver, ModelHost, ModelStopReason, ModelStream, PrepareContextHost,
        ResolveContextModel, StrategyDiagnostics, StrategyOutcome, ToolDriver,
        ToolExecutionContext, ToolHost,
    },
    timeline_tree::{BranchToolConfig, TreeCommands},
    tool_registry::ToolRegistry,
    wait::Wait,
    workspace_tools::{file_reads::FileReads, files::FileTools},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::testing::{
    branch_events, chain_events, chain_events_paged, timeline_events,
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// How many events one page of a line reads.
const LINE_PAGE: usize = 10_000;
use support::{Broadcast, Committed};

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("branch flow completed")
}
/// [`soon`] for the waits a loaded machine can stretch.
async fn patient<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("branch flow completed")
}

struct FlowModel;
impl ModelDriver for FlowModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let messages = &request.messages;
        let mut end = messages.len();
        while end > 0 && said(&messages[end - 1]).starts_with("<dim-branch-") {
            end -= 1;
        }
        if end > 0
            && let Some(commands) = messages
                .iter()
                .rev()
                .find_map(|message| said(message).strip_prefix("double:").map(str::to_owned))
        {
            let mut commands = commands.split(" | ");
            let first = commands.next().expect("double command");
            let second = commands.next().expect("double command");
            let last = &messages[end - 1];
            let answered = if last.role == Role::User {
                Some(first)
            } else if last.role == Role::ToolResult && said(last).contains("is ending") {
                Some(second)
            } else {
                None
            };
            if let Some(answered) = answered {
                let (name, input) = answered.split_once(' ').unwrap_or((answered, "{}"));
                let input: JsonObject = serde_json::from_str(input).expect("test command JSON");
                let message = uuid::Uuid::new_v4().to_string();
                let id = uuid::Uuid::new_v4().to_string();
                let chunks = vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: message.clone(),
                    }),
                    Ok(ModelChunk::ToolStart {
                        id: id.clone(),
                        name: name.into(),
                    }),
                    Ok(ModelChunk::ToolUse(ToolCall {
                        id,
                        name: name.into(),
                        input,
                    })),
                    Ok(ModelChunk::MessageStop {
                        message_id: message,
                        reason: ModelStopReason::ToolUse,
                    }),
                ];
                return Box::pin(async move {
                    Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
                });
            }
        }
        let command = end < messages.len()
            && end > 0
            && messages[end - 1].role == Role::User
            && said(&messages[end - 1]).starts_with("tool:");
        if !command {
            return EchoModel.invoke(request, connection, cancel);
        }
        let mut answered = (*request).clone();
        answered.messages.truncate(end);
        EchoModel.invoke(Arc::new(answered), connection, cancel)
    }
}
fn mock_hosts(builder: &mut AgentBuilder) {
    defaults::mock_hosts_with_model(builder, HistoryRenderer::default(), Arc::new(FlowModel));
}
fn said(message: &Message) -> String {
    let parts: Vec<String> = match &message.content {
        MessageContent::Text(text) => vec![text.clone()],
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
            .collect(),
    };
    parts.join("\n")
}
#[derive(Clone, Default)]
enum Rewrite {
    #[default]
    None,
    Business(BranchCreateAt, String),
    Identity(String),
}
#[derive(Default)]
struct Probe {
    rewrite: Mutex<Rewrite>,
    entered: Notify,
    release: Notify,
    child_entered: Notify,
    child_release: Notify,
    delay_child: AtomicBool,
    child_builds: AtomicUsize,
    child_cleanups: Arc<AtomicUsize>,
    commands: Mutex<Option<Arc<TreeCommands>>>,
    /// The Session's tool registry, so a test can call the `branch` tool itself.
    tool_registry: Mutex<Option<Arc<ToolRegistry>>>,
    /// main's Timeline as its capabilities receive it, for direct writes.
    main_timeline: Mutex<Option<Arc<Timeline>>>,
    /// A line's own Timeline: the binding its file tools keep reads in.
    child_timeline: Mutex<Option<Arc<Timeline>>>,
    /// When set, main's apply reads its first atomic batch while it opens,
    /// after a rebuild read; the outcomes go to `opening_reads`.
    read_while_opening: AtomicBool,
    opening_reads: Mutex<Vec<BatchReads>>,
    session: Mutex<Option<Session>>,
    publication_checks: Mutex<Vec<(bool, bool)>>,
    fail_child: AtomicBool,
    /// A child capability that is Active when the build fails; its cleanup
    /// writes a terminal fact and records whether the write was accepted.
    writer_ready: Notify,
    cleanup_write: Mutex<Option<bool>>,
}
/// The sequences each of [`batch_reads`] returns, or its error.
type BatchReads = Vec<std::result::Result<Vec<u64>, String>>;
/// Reads whose range ends inside (sequence 1) or at the end (2) of the
/// atomic batch committed at sequences 1 and 2.
async fn batch_reads(timeline: &Timeline) -> BatchReads {
    fn sequences(read: Result<Vec<Arc<AgentEvent>>>) -> std::result::Result<Vec<u64>, String> {
        read.map(|events| events.iter().map(|event| event.sequence).collect())
            .map_err(|error| format!("{error:?}"))
    }
    vec![
        sequences(timeline_events(timeline, TimelineQueryScope::Branch, ..=1).await),
        sequences(timeline_events(timeline, TimelineQueryScope::Chain, ..=1).await),
        sequences(timeline_events(timeline, TimelineQueryScope::Branch, ..=2).await),
        sequences(timeline_events(timeline, TimelineQueryScope::Branch, 2..=2).await),
    ]
}
struct ProbePlugin {
    probe: Arc<Probe>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
    commands: Key<TreeCommands>,
    registry: Key<ToolRegistry>,
}
impl ProbePlugin {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            timeline: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
            commands: names.key()?,
            registry: names.key()?,
        })
    }
}
impl DynPlugin for ProbePlugin {
    fn name(&self) -> &str {
        "test.branch-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.identity.erase(),
            self.commands.erase(),
            self.registry.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let commands = ctx.require_at(&self.commands)?;
            let registry = ctx.require_at(&self.registry)?;
            let probe = self.probe.clone();
            *probe.commands.lock().unwrap() = Some(commands);
            *probe.tool_registry.lock().unwrap() = Some(registry);
            if identity.branch_id == "main" {
                *probe.main_timeline.lock().unwrap() = Some(timeline.clone());
                if probe.read_while_opening.load(Ordering::SeqCst) {
                    timeline_events(
                        &timeline,
                        TimelineQueryScope::Branch,
                        ..=timeline.committed(),
                    )
                    .await?;
                    let reads = batch_reads(&timeline).await;
                    probe.opening_reads.lock().unwrap().push(reads);
                }
            } else {
                *probe.child_timeline.lock().unwrap() = Some(timeline.clone());
                probe.child_builds.fetch_add(1, Ordering::SeqCst);
                let cleanups = probe.child_cleanups.clone();
                ctx.effect(|| async move {
                    cleanups.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })?;
                probe.child_entered.notify_one();
                // Holds the child's build open until the test releases it, then
                // fails the apply: creation fails in debug and release alike.
                if probe.delay_child.load(Ordering::SeqCst) {
                    tokio::select! {
                        _ = probe.child_release.notified() => {}
                        _ = ctx.cancelled() => {}
                    }
                    return Err(rutis::Error::from(ErrorKind::PluginFailed(
                        "test.branch-probe: the held child fails its apply".into(),
                    )));
                }
                if probe.fail_child.load(Ordering::SeqCst) {
                    tokio::select! {
                        _ = probe.writer_ready.notified() => {}
                        _ = ctx.cancelled() => {}
                    }
                    return Err(rutis::Error::from(ErrorKind::ServiceNotFound(
                        "/private/agent-secret/rejected-candidate".into(),
                    )));
                }
                let observer = probe.clone();
                let branch = identity.branch_id.clone();
                timeline.before(
                    ctx,
                    "observe publication",
                    &["branch/start", "input/message"],
                    move |event| {
                        if let Some(session) = observer.session.lock().unwrap().as_ref() {
                            let listed = session.branches().iter().any(|info| info.id == branch);
                            let readable = session.sequence(&branch).is_ok();
                            observer
                                .publication_checks
                                .lock()
                                .unwrap()
                                .push((listed, readable));
                        }
                        Ok(event)
                    },
                )?;
            }
            let before = probe.clone();
            timeline.before(
                ctx,
                "rewrite branch",
                &["branch/create"],
                move |mut event| {
                    let EventKind::BranchCreate(data) = &mut event.kind else {
                        unreachable!()
                    };
                    match &*before.rewrite.lock().unwrap() {
                        Rewrite::None => {}
                        Rewrite::Business(at, prompt) => {
                            data.at = *at;
                            data.prompt = prompt.clone();
                        }
                        Rewrite::Identity(id) => data.branch = id.clone(),
                    }
                    Ok(event)
                },
            )?;
            dispatcher.subscribe(
                ctx,
                "pause before create",
                &["input/message"],
                move |event| {
                    let probe = probe.clone();
                    Box::pin(async move {
                        if matches!(&event.kind, EventKind::InputMessage(data) if matches!(
                            data.user.as_ref().map(|user| user.content.as_slice()),
                            Some([ContentBlock::Text { text }]) if text == "pause dispatcher"
                        )) {
                            probe.entered.notify_one();
                            probe.release.notified().await;
                        }
                        Ok(())
                    })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
/// Child-branch capability whose cleanup writes to the branch Timeline.
struct CleanupWriter {
    probe: Arc<Probe>,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
}
impl CleanupWriter {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            timeline: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for CleanupWriter {
    fn name(&self) -> &str {
        "test.cleanup-writer"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            if identity.branch_id == "main" || !self.probe.fail_child.load(Ordering::SeqCst) {
                return Ok(Effect::Done);
            }
            let probe = self.probe.clone();
            ctx.effect(move || async move {
                let written = timeline.append(interrupt().into()).is_ok();
                *probe.cleanup_write.lock().unwrap() = Some(written);
                Ok(())
            })?;
            self.probe.writer_ready.notify_one();
            Ok(Effect::Done)
        })
    }
}
async fn setup(probe: &Arc<Probe>) -> MinimalHost {
    MinimalHost::start(&builder(probe)).await.unwrap()
}
async fn setup_with(store: &Arc<TestStore>) -> MinimalHost {
    MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap()
}
fn builder(probe: &Arc<Probe>) -> AgentBuilder {
    builder_with(probe, defaults::install)
}

/// [`FlowModel`] with two gates: a request whose last text is `GATE-MAIN` or
/// `GATE-LINE` waits at that gate until released, then answers with an `echo`
/// call, so the turn stays running while the test drives the other line.
#[derive(Default)]
struct GateModel {
    main_entered: Notify,
    main_release: Notify,
    line_entered: Notify,
    line_release: Notify,
}
impl ModelDriver for GateModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let gate = request
            .messages
            .iter()
            .rev()
            .map(|message| said(message))
            .find(|text| !text.starts_with("<dim-branch-"))
            .and_then(|text| match text.as_str() {
                "GATE-MAIN" => Some((&self.main_entered, &self.main_release)),
                "GATE-LINE" => Some((&self.line_entered, &self.line_release)),
                _ => None,
            });
        let Some((entered, release)) = gate else {
            return FlowModel.invoke(request, connection, cancel);
        };
        Box::pin(async move {
            entered.notify_one();
            tokio::select! {
                _ = release.notified() => {}
                _ = cancel.cancelled() => {
                    return Err(HostError::Cancelled("gated model cancelled".into()));
                }
            }
            let message = uuid::Uuid::new_v4().to_string();
            let id = uuid::Uuid::new_v4().to_string();
            let input = serde_json::json!({"text": "gated"});
            let chunks = vec![
                Ok(ModelChunk::MessageStart {
                    message_id: message.clone(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: id.clone(),
                    name: "echo".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id,
                    name: "echo".into(),
                    input: input.as_object().cloned().unwrap_or_default(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: message,
                    reason: ModelStopReason::ToolUse,
                }),
            ];
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

/// One assistant message that calls `calls`, the shape a model uses to put two
/// tool calls in one turn.
fn calls_model(calls: Vec<(&str, JsonObject)>) -> ModelStream {
    let message = uuid::Uuid::new_v4().to_string();
    let mut chunks = vec![Ok(ModelChunk::MessageStart {
        message_id: message.clone(),
    })];
    for (name, input) in calls {
        let id = uuid::Uuid::new_v4().to_string();
        chunks.push(Ok(ModelChunk::ToolStart {
            id: id.clone(),
            name: name.into(),
        }));
        chunks.push(Ok(ModelChunk::ToolUse(ToolCall {
            id,
            name: name.into(),
            input,
        })));
    }
    chunks.push(Ok(ModelChunk::MessageStop {
        message_id: message,
        reason: ModelStopReason::ToolUse,
    }));
    Box::pin(futures_util::stream::iter(chunks))
}
/// [`FlowModel`] that scripts the two tool calls a test needs from `main`: an
/// `echo` call in the step after the result of a `branch` create (a later step
/// of the same turn, after the line exists), and an `echo` call in answer to a
/// message from a line.
#[derive(Default)]
struct ActingModel;
impl ModelDriver for ActingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let created = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::ToolResult)
            .is_some_and(|message| said(message).contains("Line created:"));
        let from_line = request.messages.last().is_some_and(|message| {
            message.role == Role::User && said(message).starts_with("<dim-branch-message")
        });
        if !created && !from_line {
            return FlowModel.invoke(request, connection, cancel);
        }
        Box::pin(async move {
            Ok(calls_model(vec![(
                "echo",
                serde_json::json!({"text": "acting"})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )]) as ModelStream)
        })
    }
}
/// [`FlowModel`] that answers a line's purpose with its tool call when the
/// purpose is a `tool:` command. It reads only the last message, so a parent
/// block written over the purpose hides the command, as it would from a real
/// model.
#[derive(Default)]
struct PurposeModel;
impl ModelDriver for PurposeModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let Some(last) = request.messages.last() else {
            return FlowModel.invoke(request, connection, cancel);
        };
        if last.role == Role::User
            && said(last).starts_with("<dim-branch-line ")
            && said(last).contains("tool:echo ")
        {
            return Box::pin(async move {
                Ok(calls_model(vec![(
                    "echo",
                    serde_json::json!({"text": "purpose"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                )]) as ModelStream)
            });
        }
        FlowModel.invoke(request, connection, cancel)
    }
}

fn builder_with(probe: &Arc<Probe>, install: impl FnOnce(&mut AgentBuilder)) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    mock_hosts(&mut builder);
    install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_gates(probe: &Arc<Probe>, gates: &Arc<GateModel>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), gates.clone());
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_model(probe: &Arc<Probe>, model: Arc<dyn ModelDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_store(probe: &Arc<Probe>, store: Arc<dyn TimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
/// [`builder_with_store`](builder_with_store) with the branch texts at a
/// candidate's factory configuration.
fn builder_with_store_config(
    probe: &Arc<Probe>,
    store: Arc<dyn TimelineStore>,
    config: BranchToolConfig,
) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    mock_hosts(&mut builder);
    defaults::install_with_branch_config(&mut builder, config);
    probe_features(&mut builder, probe);
    builder
}
/// [`builder_with`](builder_with) with `tools` in place of [`MockTools`].
fn builder_with_tools(probe: &Arc<Probe>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::mock_model_hosts_with_model(
        &mut builder,
        HistoryRenderer::default(),
        Arc::new(FlowModel),
    );
    defaults::tool_host(&mut builder, tools);
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}

/// An `echo` tool and nothing else, so the registry's `wait` is Wait's own
/// rather than the host [`MockTools`] declares.
struct EchoOnly;
impl ToolDriver for EchoOnly {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..dim_agent_capabilities::tool_registry::definition(
                "echo",
                "Return text without side effects.",
                serde_json::json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"text":{"type":"string"}}}),
            )
        }]
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let text = call
                .input
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(ToolResult::text(text))
        })
    }
}

/// [`builder_with`](builder_with) with Wait installed and a host that declares
/// only `echo`, so a `tool:wait` call runs the capability's `wait`.
fn builder_with_wait(probe: &Arc<Probe>, model: Arc<dyn ModelDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::mock_model_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    defaults::tool_host(&mut builder, Arc::new(EchoOnly));
    defaults::install(&mut builder);
    dim_agent_capabilities::wait::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn probe_features(builder: &mut AgentBuilder, probe: &Arc<Probe>) {
    let writer = probe.clone();
    builder.require_feature(
        Feature::new("test.cleanup-writer").branch("test.cleanup-writer", move |names| {
            Ok(Box::new(CleanupWriter::new(names, writer.clone())?))
        }),
    );
    let probe = probe.clone();
    builder.require_feature(
        Feature::new("test.branch-probe").branch("test.branch-probe", move |names| {
            Ok(Box::new(ProbePlugin::new(names, probe.clone())?))
        }),
    );
}

/// [`builder`] with the branch texts at a candidate's factory configuration
/// ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)):
/// BranchLine reads the roster's closing sentence from its member.
fn builder_with_branch_config(probe: &Arc<Probe>, config: BranchToolConfig) -> AgentBuilder {
    builder_with(probe, move |builder| {
        defaults::install_with_branch_config(builder, config)
    })
}

fn options(prompt: &str) -> BranchOptions {
    BranchOptions {
        prompt: prompt.into(),
        source: BranchSource::Host {
            operation_id: "test-branch".into(),
        },
    }
}
fn create_event(events: &[Arc<AgentEvent>]) -> Arc<AgentEvent> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::BranchCreate(_)))
        .unwrap()
        .clone()
}
/// Appends a completed turn to a hand-built Timeline and returns its end.
/// A later turn of `main` a line must hear about: the operator tells `main`
/// something, then the turn runs and ends. The operator input is what counts
/// as new activity ([timeline branches §7.1.1](../../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
fn moved_turn(timeline: &Timeline, turn: &str) -> Arc<AgentEvent> {
    timeline
        .append(
            EventKind::InputMessage(InputMessageData::text("move on", Some(InputMode::Queue)))
                .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: turn.into(),
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::TurnEnd(TurnEndData {
                turn_id: turn.into(),
                outcome: TurnOutcome::Completed {
                    reason: "stop".into(),
                },
            })
            .into(),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_settles_interrupted_creates_and_keeps_failed_directories_inactive() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let main = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let input = |text: &str| {
        EventKind::InputMessage(InputMessageData::text(text, Some(InputMode::Append))).into()
    };
    main.append_batch_registered(vec![input("first"), input("second")], |_| Ok(()))
        .unwrap();
    let failed = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("failed").source,
                branch: "failed".into(),
                prompt: "failed".into(),
            })
            .into(),
        )
        .unwrap();
    main.append(
        EventKind::BranchCreateFailed(BranchCreateFailedData::Failed {
            branch: "failed".into(),
            create_event: failed.sequence,
            error: "build failed".into(),
        })
        .into(),
    )
    .unwrap();
    let failed_child = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "failed".into(),
        },
        vec![("main".into(), failed.sequence)],
        store.clone(),
    );
    failed_child
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("unpublished child").source,
                branch: "failed-grandchild".into(),
                prompt: "unpublished child".into(),
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&failed_child)
        .await
        .unwrap();
    let pending = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("pending").source,
                branch: "pending".into(),
                prompt: "pending".into(),
            })
            .into(),
        )
        .unwrap();
    let partial = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "pending".into(),
        },
        vec![("main".into(), pending.sequence)],
        store.clone(),
    );
    partial
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(pending.sequence),
            })
            .into(),
        )
        .unwrap();
    partial
        .append(
            EventKind::InputMessage(InputMessageData {
                blocks: vec![dim_agent_capabilities::context_block::raw(
                    "<dim-state source=\"dim/environment/date\">2026-10-02</dim-state>".into(),
                    Some("date".into()),
                )],
                mode: Some(InputMode::Append),
                ..InputMessageData::default()
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&partial)
        .await
        .unwrap();
    let loading = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("loading").source,
                branch: "loading".into(),
                prompt: "loading".into(),
            })
            .into(),
        )
        .unwrap();
    let loaded = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "loading".into(),
        },
        vec![("main".into(), loading.sequence)],
        store.clone(),
    );
    loaded
        .append(
            EventKind::InputMessage(InputMessageData {
                blocks: vec![dim_agent_capabilities::context_block::raw(
                    "<dim-state source=\"test/status\">loaded</dim-state>".into(),
                    Some("status".into()),
                )],
                ..InputMessageData::text("", Some(InputMode::Append))
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&loaded)
        .await
        .unwrap();
    let orphan = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("orphan").source,
                branch: "orphan".into(),
                prompt: "orphan".into(),
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    probe.read_while_opening.store(true, Ordering::SeqCst);
    let mut builder = builder(&probe);
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    let batch = vec![Ok(vec![1]), Ok(vec![1]), Ok(vec![1, 2]), Ok(vec![2])];
    for _ in 0..2 {
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = soon(agent.restore_session("saved".into())).await.unwrap();
        soon(session.idle("main")).await.unwrap();
        let opening = probe.opening_reads.lock().unwrap().pop().unwrap();
        let main = probe.main_timeline.lock().unwrap().clone().unwrap();
        assert_eq!(opening, batch);
        assert_eq!(batch_reads(&main).await, batch);
        assert!(matches!(
            main.batch_end(1).await,
            Err(AgentError::InvalidInput(_))
        ));
        assert_eq!(
            session
                .branches()
                .iter()
                .map(|info| info.id.as_str())
                .collect::<Vec<_>>(),
            ["main"]
        );
        let history = branch_events(&session, "main", ..).await.unwrap();
        let terminals: Vec<_> = history
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::BranchCreateFailed(data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(terminals.len(), 4);
        assert!(terminals.iter().any(|data| matches!(data,
            BranchCreateFailedData::Failed { branch, create_event, .. }
            if branch == "loading" && *create_event == loading.sequence)));
        assert!(terminals.iter().any(|data| matches!(data,
            BranchCreateFailedData::Failed { branch, create_event, .. }
            if branch == "pending" && *create_event == pending.sequence)));
        assert!(terminals.iter().any(|data| matches!(data,
            BranchCreateFailedData::Failed { branch, create_event, .. }
            if branch == "orphan" && *create_event == orphan.sequence)));
        assert_eq!(probe.child_builds.load(Ordering::SeqCst), 0);
        soon(agent.close()).await.unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_completes_an_end_without_its_result() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("ended line").source,
                branch: "ended".into(),
                prompt: "ended line".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "ended".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the parser",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    let grand = child
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("grandchild").source,
                branch: "grand".into(),
                prompt: "grandchild".into(),
            })
            .into(),
        )
        .unwrap();
    let grandchild = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "grand".into(),
        },
        vec![
            ("ended".into(), grand.sequence),
            ("main".into(), create.sequence),
        ],
        store.clone(),
    );
    grandchild
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("ended".into()),
                create_event: Some(grand.sequence),
            })
            .into(),
        )
        .unwrap();
    let grand_first = grandchild
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the grandchild",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&grandchild)
        .await
        .unwrap();
    let source = BranchSource::Host {
        operation_id: "user-op".into(),
    };
    let end = child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: source.clone(),
                ancestor: None,
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    let mut builder = builder(&probe);
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        session
            .branches()
            .iter()
            .map(|info| info.id.as_str())
            .collect::<Vec<_>>(),
        ["main"]
    );
    let results: Vec<BranchEndedData> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == "ended" => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "{results:#?}");
    assert_eq!(results[0].source, source);
    assert_eq!(results[0].end_event, end.sequence);
    assert!(results[0].ancestor.is_none());
    assert_eq!(session.ended_head("ended"), Some(end.sequence));
    let page = line_events(&session, "ended").await.unwrap();
    assert!(
        page.iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnd(_)))
    );
    assert!(matches!(
        soon(session.end_branch("ended", source.clone())).await,
        Err(AgentError::BranchEnded(line)) if line == "ended"
    ));
    assert_eq!(session.ended_head("grand"), Some(grand_first.sequence));
    assert!(
        line_events(&session, "grand")
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchStart(_)))
    );
    assert!(matches!(
        soon(session.end_branch("grand", source.clone())).await,
        Err(AgentError::BranchEnded(line)) if line == "grand"
    ));
    assert_eq!(
        session.branches().iter().find(|info| info.id == "grand"),
        None,
        "an ended line leaves the open directory"
    );
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    let info = commands
        .info("grand".into())
        .expect("an ended line keeps its record");
    assert_eq!(info.source, None);
    assert_eq!(
        commands
            .display("grand".into())
            .await
            .unwrap()
            .entry
            .info
            .source,
        Some(options("grandchild").source)
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_completes_an_interrupted_descendants_end_first() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let key = |branch: &str| TimelineKey {
        session_id: "saved".into(),
        branch_id: branch.into(),
    };
    let main = Timeline::new(root(), key("main"), vec![], store.clone());
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("host line").source,
                branch: "X".into(),
                prompt: "host line".into(),
            })
            .into(),
        )
        .unwrap();
    let line = Timeline::new(
        root(),
        key("X"),
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    line.append(
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        })
        .into(),
    )
    .unwrap();
    line.append(
        EventKind::InputMessage(InputMessageData::text(
            "work on the line",
            Some(InputMode::Queue),
        ))
        .into(),
    )
    .unwrap();
    let child_create = line
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("child").source,
                branch: "C".into(),
                prompt: "child".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        root(),
        key("C"),
        vec![
            ("X".into(), child_create.sequence),
            ("main".into(), create.sequence),
        ],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("X".into()),
                create_event: Some(child_create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the child",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    let source = BranchSource::Host {
        operation_id: "op:end".into(),
    };
    let end = line
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: source.clone(),
                ancestor: None,
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    let child_end = child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: source.clone(),
                ancestor: Some("X".into()),
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    for timeline in [&child, &line, &main] {
        dim_agent_runtime::core::close_timeline(timeline)
            .await
            .unwrap();
    }
    let mut builder = builder(&Arc::new(Probe::default()));
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        session
            .branches()
            .iter()
            .map(|info| info.id.as_str())
            .collect::<Vec<_>>(),
        ["main"]
    );
    assert_eq!(session.ended_head("C"), Some(child_end.sequence));
    assert_eq!(
        session.ended_head("X"),
        Some(end.sequence + 1),
        "the child's result extends the line"
    );
    let events = line_events(&session, "X").await.unwrap();
    let result = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == "C"))
        .expect("the child's result is completed on its parent");
    assert_eq!(result.sequence, end.sequence + 1);
    assert_eq!(
        result.kind,
        EventKind::BranchEnded(BranchEndedData {
            branch: "C".into(),
            end_event: child_end.sequence,
            source: source.clone(),
            ancestor: Some("X".into()),
        })
    );
    let results: Vec<BranchEndedData> = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == "X" => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [BranchEndedData {
            branch: "X".into(),
            end_event: end.sequence,
            source: source.clone(),
            ancestor: None,
        }]
    );
    soon(agent.close()).await.unwrap();
}

/// A memory store for tests that records every `query`, and can fail reads on
/// purpose. [`refusing_ranges`](TestStore::refusing_ranges) refuses a restore's
/// range reads (`read`, `read_page`, `read_segments`), so a test can assert
/// that a restored line is read by selections alone; an [armed](TestStore::arm)
/// store fails one line's opening query, so a creation can be made to fail at
/// that read
/// ([§7.1.1](../../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
/// Every other read, like the boundary checks a creation makes of a line,
/// keeps working, so a refusal lands on the read it was aimed at.
#[derive(Default)]
struct TestStore {
    memory: Arc<MemoryTimelineStore>,
    queries: Mutex<Vec<(String, Vec<String>)>>,
    reads: AtomicUsize,
    armed: Mutex<Option<String>>,
    refuse_ranges: bool,
    /// The branch whose next final-message delivery answers its publish
    /// callback with an error, the failure shape a store reports after
    /// accepting a batch.
    fail_delivery: Mutex<Option<String>>,
    /// The branch whose next submission lookup fails at the read, before the
    /// store writes anything: a transitory failure a delivery can retry.
    fail_lookup: Mutex<Option<String>>,
    /// The hold seam and the queue it keeps back, under one lock so arming a
    /// hold and deciding that an append queues behind it cannot interleave.
    seam: Mutex<Seam>,
    /// Wakes the test that waits for a hold to be taken.
    holding: tokio::sync::Notify,
}

/// One batch's admission: its events and the callback that answers it.
type QueuedBatch = (TimelineKey, Vec<StoredEvent>, PublishCallback);

/// A store's hold seam: the next append to a branch that it accepts and does
/// not commit, the batch that append became, and every later append to the same
/// branch, in the order the store accepted them. `TimelineStore` runs one
/// branch's publish callbacks in sequence order (`timeline_store.rs:75`), so a
/// batch behind another may not commit first.
#[derive(Default)]
struct Seam {
    /// The branch whose next append of a kind is held. The kind is an event key;
    /// `branch-final` stands for a tool end's final message on the parent.
    armed: Option<(String, String)>,
    front: Option<QueuedBatch>,
    behind: Vec<QueuedBatch>,
}
impl TestStore {
    /// A store over `memory` that refuses every range read.
    fn refusing_ranges(memory: Arc<MemoryTimelineStore>) -> Self {
        Self {
            memory,
            refuse_ranges: true,
            ..Self::default()
        }
    }
    /// Fails `query` on `branch` while it selects the kinds a line's opening
    /// reads.
    fn arm(&self, branch: &str) {
        *self.armed.lock().unwrap() = Some(branch.into());
    }
    fn disarm(&self) {
        *self.armed.lock().unwrap() = None;
    }
    /// Fails the next `branch-final:` delivery to `branch` through its
    /// publish callback.
    fn fail_next_delivery(&self, branch: &str) {
        *self.fail_delivery.lock().unwrap() = Some(branch.into());
    }
    /// Fails the next submission lookup on `branch`, then answers normally.
    fn fail_next_lookup(&self, branch: &str) {
        *self.fail_lookup.lock().unwrap() = Some(branch.into());
    }
    /// Accepts the next `branch/default` append to `branch` and holds its
    /// publication, so a caller can assert what the tree answers before the
    /// commit. Every later append to that branch queues behind the held batch
    /// until it settles.
    fn hold_next_default(&self, branch: &str) {
        self.seam.lock().unwrap().armed = Some((branch.into(), "branch/default".into()));
    }
    /// Accepts the next final message a tool end delivers to `branch` and holds
    /// its commit. Every later append to that branch queues behind it.
    fn hold_next_final(&self, branch: &str) {
        self.seam.lock().unwrap().armed = Some((branch.into(), "branch-final".into()));
    }
    /// Waits until a held publication is there to release.
    async fn wait_holding(&self) {
        loop {
            let notified = self.holding.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.seam.lock().unwrap().front.is_some() {
                return;
            }
            notified.await;
        }
    }
    /// Writes the held batch and answers it as committed, then commits what the
    /// store accepted behind it, in the order it came: the writer drains its
    /// queue once the batch in front settles.
    fn release(&self) {
        let mut seam = self.seam.lock().unwrap();
        let (key, events, publish) = seam.front.take().expect("a held append");
        self.memory.append(key, events, publish).unwrap();
        for (key, events, publish) in std::mem::take(&mut seam.behind) {
            self.memory.append(key, events, publish).unwrap();
        }
    }
    /// Answers the held append as a failed transaction that wrote nothing and
    /// settles every batch already queued behind it in the same Session with
    /// the same failure. A later append of that Session writes directly, and
    /// another Session queued on the same line still commits.
    fn release_failed(&self) {
        let mut seam = self.seam.lock().unwrap();
        let (front, _, publish) = seam.front.take().expect("a held append");
        let error = AgentError::Storage("injected transaction failure".into());
        publish(Err(error.clone()));
        for (key, events, publish) in std::mem::take(&mut seam.behind) {
            if key.session_id == front.session_id {
                publish(Err(error.clone()));
            } else {
                self.memory.append(key, events, publish).unwrap();
            }
        }
    }
    /// The pages `branch` was read with while selecting exactly `kinds`.
    fn pages(&self, branch: &str, kinds: &[&str]) -> usize {
        self.queries
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, selected)| {
                line == branch
                    && selected.len() == kinds.len()
                    && selected
                        .iter()
                        .map(String::as_str)
                        .eq(kinds.iter().copied())
            })
            .count()
    }
    fn range_reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
    fn refused<T>(&self) -> Result<T> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Storage("a restore read a stored range".into()))
    }
}
impl TimelineStore for TestStore {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        fn kinds(filter: &dim_agent_runtime::QueryFilter) -> Vec<String> {
            use dim_agent_runtime::{QueryFilter, QueryOperand};
            match filter {
                QueryFilter::In {
                    value: QueryOperand::Field { path },
                    values,
                } if path == "kind" => values
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect(),
                QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                    conditions.iter().flat_map(kinds).collect()
                }
                _ => vec![],
            }
        }
        Box::pin(async move {
            let selected = query.filter.as_ref().map(kinds).unwrap_or_default();
            if self.armed.lock().unwrap().as_deref() == Some(scope.key.branch_id.as_str())
                && selected.as_slice() == ["input/message"]
            {
                return Err(AgentError::Storage("injected opening read failure".into()));
            }
            let lookup = matches!(
                &query.select,
                dim_agent_runtime::QuerySelect::Fields { fields }
                    if fields.iter().map(String::as_str).eq(["id", "position.sequence"])
            );
            if lookup {
                let mut armed = self.fail_lookup.lock().unwrap();
                if armed.as_deref() == Some(scope.key.branch_id.as_str()) {
                    *armed = None;
                    return Err(AgentError::Storage("injected lookup failure".into()));
                }
            }
            self.queries
                .lock()
                .unwrap()
                .push((scope.key.branch_id.clone(), selected));
            self.memory.query(scope, query, page).await
        })
    }

    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> dim_agent_runtime::BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        let final_message = |record: &StoredEvent| match &record.event.kind {
            EventKind::InputMessage(data) => data
                .submission_id
                .as_deref()
                .is_some_and(|id| id.starts_with("branch-final:")),
            _ => false,
        };
        let mut seam = self.seam.lock().unwrap();
        let hold = seam.armed.as_ref().is_some_and(|(branch, kind)| {
            branch == &key.branch_id
                && if kind == "branch-final" {
                    events.iter().any(final_message)
                } else {
                    events.iter().any(|record| record.event.kind.key() == kind)
                }
        });
        if hold {
            seam.armed = None;
            seam.front = Some((key, events, publish));
            drop(seam);
            self.holding.notify_waiters();
            return Ok(());
        }
        if seam
            .front
            .as_ref()
            .is_some_and(|(held, _, _)| held.branch_id == key.branch_id)
        {
            seam.behind.push((key, events, publish));
            return Ok(());
        }
        drop(seam);
        let delivery = events.iter().any(final_message);
        let failed = delivery && {
            let mut armed = self.fail_delivery.lock().unwrap();
            let failed = armed.as_deref() == Some(key.branch_id.as_str());
            if failed {
                *armed = None;
            }
            failed
        };
        if failed {
            publish(Err(AgentError::Storage(
                "injected append failure after the batch was accepted".into(),
            )));
            return Ok(());
        }
        self.memory.append(key, events, publish)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<StoredEvent>>> {
        if self.refuse_ranges {
            return Box::pin(async move { self.refused() });
        }
        self.memory.read_page(key, after, limit, through)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        if self.refuse_ranges {
            return Box::pin(async move { self.refused() });
        }
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.memory.latest_snapshot(key, through, interpreter)
    }
}

/// #1064: a restore resolves a line's own end from one selection page over the
/// whole line, and reads no stored range. `A` has an end its parent never
/// recorded (the restore must complete it from `branch/end`); `B` has none and
/// is long enough that a window grown 32, 256, 2048 … would need four pages
/// and four round trips to answer the same question.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_reads_a_line_end_in_one_page_and_no_range() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let key = |branch: &str| TimelineKey {
        session_id: "saved".into(),
        branch_id: branch.into(),
    };
    let source = BranchSource::Host {
        operation_id: "op:restore".into(),
    };
    let main = Timeline::new(root(), key("main"), vec![], store.clone());
    let create_a = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: source.clone(),
                branch: "A".into(),
                prompt: "a".into(),
            })
            .into(),
        )
        .unwrap();
    let a = Timeline::new(
        root(),
        key("A"),
        vec![("main".into(), create_a.sequence)],
        store.clone(),
    );
    a.append(
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create_a.sequence),
        })
        .into(),
    )
    .unwrap();
    a.append(
        EventKind::InputMessage(InputMessageData::text("work on a", Some(InputMode::Queue))).into(),
    )
    .unwrap();
    let a_end = a
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: source.clone(),
                ancestor: None,
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    let create_b = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: source.clone(),
                branch: "B".into(),
                prompt: "b".into(),
            })
            .into(),
        )
        .unwrap();
    let b = Timeline::new(
        root(),
        key("B"),
        vec![("main".into(), create_b.sequence)],
        store.clone(),
    );
    b.append(
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create_b.sequence),
        })
        .into(),
    )
    .unwrap();
    b.append(
        EventKind::InputMessage(InputMessageData::text("work on b", Some(InputMode::Queue))).into(),
    )
    .unwrap();
    for index in 0..4_000 {
        b.append(
            EventKind::TurnEnd(TurnEndData {
                turn_id: format!("pad-{index}"),
                outcome: TurnOutcome::Completed {
                    reason: "padding".into(),
                },
            })
            .into(),
        )
        .unwrap();
    }
    for timeline in [&b, &a, &main] {
        dim_agent_runtime::core::close_timeline(timeline)
            .await
            .unwrap();
    }

    let counting = Arc::new(TestStore::refusing_ranges(store));
    let builder = builder_with_store(&Arc::new(Probe::default()), counting.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();

    assert_eq!(counting.range_reads(), 0, "a restore reads no stored range");
    assert_eq!(
        session.ended_head("A"),
        Some(a_end.sequence),
        "the interrupted end is resolved from the line itself"
    );
    let results: Vec<BranchEndedData> = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == "A" => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [BranchEndedData {
            branch: "A".into(),
            end_event: a_end.sequence,
            source: source.clone(),
            ancestor: None,
        }]
    );
    let pages = counting.pages("B", &["branch/start", "branch/end"]);
    assert_eq!(
        pages, 1,
        "a line with no end costs one page, whatever its length; the restore asked {pages} times"
    );
    soon(agent.close()).await.unwrap();
}

struct SettleOnActivation {
    child: String,
    source: BranchSource,
    ends: Arc<Mutex<Vec<String>>>,
    tree: Key<TreeCommands>,
    session: Key<SessionLifecycle>,
    identity: Key<TimelineIdentity>,
}
impl SettleOnActivation {
    fn new(
        names: &Names<'_>,
        child: String,
        source: BranchSource,
        ends: Arc<Mutex<Vec<String>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            child,
            source,
            ends,
            tree: names.key()?,
            session: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for SettleOnActivation {
    fn name(&self) -> &str {
        "test.settle-on-activation"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.tree.erase(),
            self.session.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let session = ctx.require_at(&self.session)?;
            let identity = ctx.require_at(&self.identity)?;
            let child = self.child.clone();
            if identity.branch_id != "main" {
                return Ok(Effect::Done);
            }
            let source = self.source.clone();
            let ends = self.ends.clone();
            let tasks = TaskTracker::new();
            tasks.spawn(async move {
                if !session.activated().await {
                    return;
                }
                let result = tree.end(child, source).await;
                ends.lock().unwrap().push(format!("{result:?}"));
            });
            let tracker = tasks.clone();
            ctx.effect(move || async move {
                tracker.close();
                tracker.wait().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_at_load_finds_the_restored_line_published() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("evaluate").source,
                branch: "E".into(),
                prompt: "evaluate".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "E".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    let head = child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "judge the report",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();

    let ends = Arc::new(Mutex::new(Vec::new()));
    let mut builder = builder(&Arc::new(Probe::default()));
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    let recorded = ends.clone();
    builder.require_feature(Feature::new("test.settle-on-activation").branch(
        "test.settle-on-activation",
        move |names| {
            Ok(Box::new(SettleOnActivation::new(
                names,
                "E".into(),
                options("evaluate").source,
                recorded.clone(),
            )?))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    soon(async {
        while ends.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert_eq!(
        ends.lock().unwrap().as_slice(),
        ["Ok(())"],
        "the end at load found its line"
    );
    let main_events = branch_events(&session, "main", ..).await.unwrap();
    let results: Vec<BranchEndedData> = main_events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == "E" => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "{results:#?}");
    assert_eq!(results[0].source, options("evaluate").source);
    assert!(!main_events.iter().any(|event| matches!(
        &event.kind,
        EventKind::BranchCreateFailed(BranchCreateFailedData::Failed { branch, .. })
            if branch == "E"
    )));
    let stopped = session.ended_head("E").expect("the line ended");
    assert!(stopped > head.sequence);
    assert!(
        line_events(&session, "E")
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnd(_)))
    );
    soon(agent.close()).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_refuses_a_line_that_is_still_opening() {
    let probe = Arc::new(Probe::default());
    probe.delay_child.store(true, Ordering::SeqCst);
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let creating = tokio::spawn({
        let session = session.clone();
        async move { soon(session.branch("main".into(), cut, options("delayed"))).await }
    });
    soon(probe.child_entered.notified()).await;
    let child = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::BranchCreate(data) => Some(data.branch.clone()),
            _ => None,
        })
        .expect("the create is committed");
    assert!(matches!(
        session
            .end_branch(
                &child,
                BranchSource::Host {
                    operation_id: "test-end".into()
                }
            )
            .await,
        Err(AgentError::UnknownBranch(line)) if line == child
    ));
    probe.child_release.notify_one();
    assert!(creating.await.unwrap().is_err());
    soon(session.idle("main")).await.unwrap();
    let events = branch_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::BranchCreateFailed(BranchCreateFailedData::Failed { branch, .. })
            if branch == &child
    )));
    assert!(!events.iter().any(|event| matches!(
        &event.kind,
        EventKind::BranchEnded(data) if data.branch == child
    )));
    assert!(matches!(
        session
            .end_branch(
                &child,
                BranchSource::Host {
                    operation_id: "test-end".into()
                }
            )
            .await,
        Err(AgentError::UnknownBranch(line)) if line == child
    ));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_that_is_ending_opens_no_more_lines() {
    let store = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let mut builder = builder(&probe);
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "hold the dispatcher").await;
    soon(session.idle("main")).await.unwrap();
    soon(session.idle(&child)).await.unwrap();
    session
        .submit("pause dispatcher".into(), InputMode::Append, &child)
        .await
        .unwrap();
    soon(probe.entered.notified()).await;
    let ending = tokio::spawn({
        let session = session.clone();
        let child = child.clone();
        async move {
            session
                .end_branch(
                    &child,
                    BranchSource::Host {
                        operation_id: "test-end".into(),
                    },
                )
                .await
        }
    });
    let key = TimelineKey {
        session_id: session.id().to_owned(),
        branch_id: child.clone(),
    };
    soon(async {
        loop {
            let head = store.head(&key).await.unwrap();
            let ended = store
                .read(&key, 1..=head)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(&event.event.kind, EventKind::BranchEnd(_)));
            if ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        matches!(
            soon(session.branch(child.clone(), 0, options("grandchild"))).await,
            Err(AgentError::BranchEnded(line)) if line == child
        ),
        "the end is committed while its close waits on the held dispatcher, so a create under it is refused"
    );
    probe.release.notify_one();
    soon(ending).await.unwrap().unwrap();
    assert!(session.ended_head(&child).is_some());
    soon(agent.close()).await.unwrap();
}

async fn pause(session: &Session, probe: &Probe) {
    session
        .submit("pause dispatcher".into(), InputMode::Append, "main")
        .await
        .unwrap();
    soon(probe.entered.notified()).await;
}
/// The create is committed while the probe holds dispatch.
async fn wait_create(session: &Session, after: u64) -> Arc<AgentEvent> {
    let mut events = Committed::after(session, "main", after);
    soon(async {
        loop {
            let event = events.next().await;
            if matches!(event.kind, EventKind::BranchCreate(_)) {
                return event;
            }
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_line_ends_its_children_first_and_records_one_result_each() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("ancestor".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let parent = soon(session.branch("main".into(), cut, options("parent")))
        .await
        .unwrap();
    soon(session.idle(&parent)).await.unwrap();
    let grandchild = soon(session.branch(parent.clone(), 0, options("grandchild")))
        .await
        .unwrap();
    soon(session.idle(&grandchild)).await.unwrap();
    let ends_before = (
        session.sequence("main").unwrap(),
        session.sequence(&parent).unwrap(),
        session.sequence(&grandchild).unwrap(),
    );

    let source = BranchSource::Host {
        operation_id: "op:end".into(),
    };
    let root = session.end_branch("main", source.clone()).await;
    assert!(
        matches!(&root, Err(AgentError::InvalidInput(message)) if message.contains("cannot end")),
        "the root never ends: {root:?}"
    );
    assert!(matches!(
        session.end_branch("missing", source.clone()).await,
        Err(AgentError::UnknownBranch(_))
    ));
    assert_eq!(
        (
            session.sequence("main").unwrap(),
            session.sequence(&parent).unwrap(),
            session.sequence(&grandchild).unwrap(),
        ),
        ends_before,
        "a refused end writes nothing"
    );

    soon(session.end_branch(&parent, source.clone()))
        .await
        .unwrap();
    assert!(matches!(
        session.end_branch(&parent, source.clone()).await,
        Err(AgentError::BranchEnded(_))
    ));
    assert!(matches!(
        session
            .submit("late".into(), InputMode::Queue, &parent)
            .await,
        Err(AgentError::BranchEnded(_))
    ));
    assert!(!session.branches().iter().any(|line| line.id == parent));
    assert!(!session.branches().iter().any(|line| line.id == grandchild));

    let grandchild_events = line_events(&session, &grandchild).await.unwrap();
    assert!(matches!(
        grandchild_events
            .iter()
            .find(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .map(|event| &event.kind),
        Some(EventKind::BranchEnd(BranchEndData {
            source: end,
            ancestor,
            ..
        })) if *end == source && ancestor.as_deref() == Some(parent.as_str())
    ));
    let parent_events = line_events(&session, &parent).await.unwrap();
    assert!(matches!(
        parent_events
            .iter()
            .find(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .map(|event| &event.kind),
        Some(EventKind::BranchEnd(BranchEndData {
            source: end,
            ancestor,
            ..
        })) if *end == source && ancestor.is_none()
    ));
    let child_result = parent_events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::BranchEnded(data)
            if data.branch == grandchild && data.ancestor.as_deref() == Some(parent.as_str())
                && data.source == source)
        })
        .expect("the child's result is recorded on its parent");
    let closed = parent_events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::StopInterrupt(data)
            if data.scope == InterruptScope::Branch)
        })
        .expect("the close interrupts the line");
    assert!(
        child_result < closed,
        "the child's result precedes the parent's close"
    );
    assert!(
        chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data)
            if data.branch == parent && data.source == source && data.ancestor.is_none()))
    );
    assert_eq!(
        session.sequence(&parent).unwrap(),
        parent_events.last().unwrap().sequence
    );
    soon(agent.close()).await.unwrap();
}

/// A branch identity this Session never had stays unknown: the lifecycle
/// lookup decides open, ended and unknown in one tree read, so an unknown
/// line is never reported as an ended one (session catalog §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_this_session_never_had_is_unknown() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    assert!(matches!(
        session.state("never"),
        Err(AgentError::UnknownBranch(line)) if line == "never"
    ));
    assert!(matches!(
        session.sequence("never"),
        Err(AgentError::UnknownBranch(line)) if line == "never"
    ));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ancestors_end_closes_a_descendants_pending_tool_end() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let ancestor = soon(session.branch("main".into(), 0, options("ancestor")))
        .await
        .unwrap();
    soon(session.idle(&ancestor)).await.unwrap();
    let child = soon(session.branch(
        ancestor.clone(),
        0,
        BranchOptions {
            prompt: "tool line".into(),
            source: BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    session
        .submit(
            r#"double:branch {"action":"end","content":"the work is done"} | wait {"millis":20000}"#
                .into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    soon(async {
        loop {
            if tool_results(&session, &child)
                .await
                .iter()
                .any(|result| matches!(&result.content, ToolContent::Text(text) if text.contains("is ending")))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let source = BranchSource::Host {
        operation_id: "op:end".into(),
    };
    soon(session.end_branch(&ancestor, source.clone()))
        .await
        .unwrap();
    let events = local_events(&session, &child).await;
    let end = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .expect("the line ended itself");
    assert!(
        matches!(
            events
                .iter()
                .rev()
                .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                .map(|event| &event.kind),
            Some(EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            }))
        ),
        "the ancestor's end interrupts the line's turn"
    );

    let results: Vec<BranchEndedData> = local_events(&session, &ancestor)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == child => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [BranchEndedData {
            branch: child.clone(),
            end_event: end.0,
            source: end.1.source,
            ancestor: None,
        }],
        "the line's own end is the one that is closed"
    );
    assert_eq!(
        session.ended_head(&child),
        events.last().map(|event| event.sequence),
        "the line reads up to its close"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ancestors_end_names_itself_on_every_descendant() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let ancestor = soon(session.branch("main".into(), 0, options("ancestor")))
        .await
        .unwrap();
    soon(session.idle(&ancestor)).await.unwrap();
    let child = soon(session.branch(ancestor.clone(), 0, options("child")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let grandchild = soon(session.branch(child.clone(), 0, options("grandchild")))
        .await
        .unwrap();
    soon(session.idle(&grandchild)).await.unwrap();
    let source = BranchSource::Host {
        operation_id: "op:end".into(),
    };
    soon(session.end_branch(&ancestor, source.clone()))
        .await
        .unwrap();
    for (line, parent) in [(&child, &ancestor), (&grandchild, &child)] {
        let end = local_events(&session, line)
            .await
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::BranchEnd(data) => Some((event.sequence, data.clone())),
                _ => None,
            })
            .expect("the line ended with its ancestor");
        assert_eq!(
            (end.1.source, end.1.ancestor.as_deref()),
            (source.clone(), Some(ancestor.as_str())),
            "{} carries the ending line as its ancestor",
            line
        );
        let results: Vec<BranchEndedData> = local_events(&session, parent)
            .await
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::BranchEnded(data) if data.branch == *line => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 1, "{line}");
        assert_eq!(results[0].end_event, end.0);
        assert_eq!(results[0].source, source);
        assert_eq!(results[0].ancestor.as_deref(), Some(ancestor.as_str()));
    }
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_that_ended_itself_names_itself_on_its_descendants() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let line = soon(session.branch(
        "main".into(),
        0,
        BranchOptions {
            prompt: "tool line".into(),
            source: BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let child = soon(session.branch(line.clone(), 0, options("child")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let result = tool_call(
        &session,
        &line,
        r#"{"action":"end","content":"the work is done"}"#,
    )
    .await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    until_events(&session, &line, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnded(_)))
    })
    .await;
    let own = local_events(&session, &line)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .expect("the line ended itself");
    assert!(matches!(own.1.source, BranchSource::Tool { .. }));
    assert_eq!(own.1.ancestor, None);
    let descendant = local_events(&session, &child)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .expect("the descendant ended with its line");
    assert_eq!(
        (descendant.1.source, descendant.1.ancestor),
        (own.1.source.clone(), Some(line.clone()))
    );
    let ended: Vec<BranchEndedData> = local_events(&session, &line)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ended,
        [BranchEndedData {
            branch: child.clone(),
            end_event: descendant.0,
            source: own.1.source,
            ancestor: Some(line.clone()),
        }],
        "the descendant carries the line's own end"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_cannot_open_under_an_ending_ancestor() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let host = soon(session.branch("main".into(), cut, options("H")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    let first = soon(session.branch(host.clone(), 0, options("A")))
        .await
        .unwrap();
    soon(session.idle(&first)).await.unwrap();
    let second = soon(session.branch(host.clone(), 0, options("B")))
        .await
        .unwrap();
    soon(session.idle(&second)).await.unwrap();
    let (closing, open) = if first < second {
        (first.clone(), second.clone())
    } else {
        (second.clone(), first.clone())
    };
    session
        .submit("pause dispatcher".into(), InputMode::Append, &closing)
        .await
        .unwrap();
    soon(probe.entered.notified()).await;
    let ending = tokio::spawn({
        let session = session.clone();
        let host = host.clone();
        async move {
            session
                .end_branch(
                    &host,
                    BranchSource::Host {
                        operation_id: "test-end".into(),
                    },
                )
                .await
        }
    });
    until_events(&session, &closing, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_)))
    })
    .await;
    let late = session.branch(open.clone(), 0, options("late")).await;
    assert!(
        matches!(&late, Err(AgentError::BranchEnded(line)) if line == &host),
        "{late:?}"
    );
    probe.release.notify_one();
    soon(ending).await.unwrap().unwrap();
    for line in [&host, &first, &second] {
        assert!(session.ended_head(line).is_some(), "{line} ended");
    }
    let open: Vec<String> = session
        .branches()
        .into_iter()
        .map(|info| info.id)
        .filter(|id| id != "main")
        .collect();
    assert!(open.is_empty(), "{open:?}");
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    let never: Arc<
        dyn Fn() -> rutis::host::BoxFuture<'static, std::result::Result<(), Error>> + Send + Sync,
    > = Arc::new(|| Box::pin(async { std::future::pending().await }));
    let late = commands
        .create(host.clone(), 0, options("late"), never)
        .await;
    assert!(
        matches!(&late, Err(AgentError::BranchEnded(line)) if line == &host),
        "{late:?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ending_a_line_waits_for_a_child_still_building() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let host = soon(session.branch("main".into(), cut, options("H")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    probe.delay_child.store(true, Ordering::SeqCst);
    let builds = probe.child_builds.load(Ordering::SeqCst);
    let creating = tokio::spawn({
        let session = session.clone();
        let host = host.clone();
        async move { session.branch(host, 0, options("building")).await }
    });
    soon(async {
        while probe.child_builds.load(Ordering::SeqCst) == builds {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let ending = tokio::spawn({
        let session = session.clone();
        let host = host.clone();
        async move {
            session
                .end_branch(
                    &host,
                    BranchSource::Host {
                        operation_id: "test-end".into(),
                    },
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!ending.is_finished(), "the end waits for the child's build");
    probe.child_release.notify_one();
    let created = soon(creating).await;
    assert!(
        matches!(created, Ok(Err(_))),
        "the build failed: {created:?}"
    );
    soon(ending).await.unwrap().unwrap();
    let events = local_events(&session, &host).await;
    let failed = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)))
        .expect("the failed creation is recorded");
    let interrupted = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .expect("the close interrupts the line");
    assert!(
        failed < interrupted,
        "the build settled before the end closed the line"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnded(_))),
        "a failed creation opens nothing to end"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_child_of_an_ended_parent_stays_unknown_after_a_restart() {
    let store = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let mut build = builder(&probe);
    build.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    let agent = MinimalHost::start(&build).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let host = soon(session.branch("main".into(), cut, options("H")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    probe.delay_child.store(true, Ordering::SeqCst);
    let builds = probe.child_builds.load(Ordering::SeqCst);
    let creating = tokio::spawn({
        let session = session.clone();
        let host = host.clone();
        async move { session.branch(host, 0, options("building")).await }
    });
    soon(async {
        while probe.child_builds.load(Ordering::SeqCst) == builds {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let ending = tokio::spawn({
        let session = session.clone();
        let host = host.clone();
        async move {
            session
                .end_branch(
                    &host,
                    BranchSource::Host {
                        operation_id: "test-end".into(),
                    },
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    probe.child_release.notify_one();
    assert!(
        matches!(soon(creating).await, Ok(Err(_))),
        "the build failed"
    );
    soon(ending).await.unwrap().unwrap();
    let events = local_events(&session, &host).await;
    let child = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchCreate(data) => Some(data.branch.clone()),
            _ => None,
        })
        .expect("the create is recorded");
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    assert_eq!(session.ended_head(&child), None);
    assert!(commands.info(child.clone()).is_err());
    let id = session.id().to_owned();
    soon(agent.close()).await.unwrap();

    let probe = Arc::new(Probe::default());
    let mut build = builder(&probe);
    build.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store));
    let agent = MinimalHost::start(&build).await.unwrap();
    let session = soon(agent.restore_session(id)).await.unwrap();
    assert_eq!(session.ended_head(&child), None);
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    assert!(commands.info(child.clone()).is_err());
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_opens_with_its_identity_block_and_the_creator_prompt() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = soon(session.branch("main".into(), 0, options("what this line is for")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let local = chain_events(&session, &child, ..).await.unwrap();
    let Some(EventKind::InputMessage(input)) = local
        .iter()
        .find(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .map(|event| &event.kind)
    else {
        panic!("the child's opening input is present");
    };
    let [InputBlock::Raw { text, key, retire }] = input.blocks.as_slice() else {
        panic!("one identity block");
    };
    assert_eq!(key.as_deref(), Some("branch:line"));
    assert!(!retire);
    assert_eq!(
        text,
        &format!(
            "<dim-branch-line branch=\"{child}\" parent=\"main\" source=\"host\">\n\
             You are line {child} of this session, opened from line main at this point of its history. You are the same agent as main: everything above is your own history, and you continue from here on your own line, in parallel with main. You can read main's work after this point. Messages above that were addressed to main are main's, not yours. The message after this one says what this line is for.\n\
             </dim-branch-line>"
        )
    );
    assert_eq!(input.effective_mode(), InputMode::Queue);
    assert!(matches!(
        input.user.as_ref().map(|user| user.content.as_slice()),
        Some([ContentBlock::Text { text }]) if text == "what this line is for"
    ));
    let timeline = probe.child_timeline.lock().unwrap().clone().unwrap();
    let input = |text: &str| {
        EventKind::InputMessage(InputMessageData::text(text, Some(InputMode::Append))).into()
    };
    let batch = timeline
        .append_batch_registered(vec![input("first"), input("second")], |_| Ok(()))
        .unwrap();
    session
        .end_branch(
            &child,
            BranchSource::Host {
                operation_id: "end-batch".into(),
            },
        )
        .await
        .unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    assert!(matches!(
        tree.batch_end(child.clone(), batch[0].sequence).await,
        Err(AgentError::InvalidInput(_))
    ));
    tree.batch_end(child.clone(), batch[1].sequence)
        .await
        .unwrap();
    let page = tree
        .page(child, batch[0].sequence - 1, 1, batch[1].sequence)
        .await
        .unwrap();
    assert_eq!(
        page.len(),
        2,
        "the ended line returns the whole atomic batch"
    );
    assert_eq!(page[1].id, batch[1].id);
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_reports_the_source_its_create_recorded_open_or_restored() {
    let store = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let writer = probe.clone();
    builder.require_feature(
        Feature::new("test.branch-probe").branch("test.branch-probe", move |names| {
            Ok(Box::new(ProbePlugin::new(names, writer.clone())?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("ancestor".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let high = session.sequence("main").unwrap();
    let host = BranchSource::Host {
        operation_id: "op:1".into(),
    };
    let child = soon(session.branch(
        "main".into(),
        high,
        BranchOptions {
            prompt: "work".into(),
            source: host.clone(),
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let source = |infos: Vec<TimelineInfo>, id: &str| {
        infos.into_iter().find(|info| info.id == id).unwrap().source
    };
    assert_eq!(source(session.branches(), &child), Some(host.clone()));
    assert_eq!(source(session.branches(), "main"), None);
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    assert_eq!(
        commands.info(child.clone()).unwrap().source,
        Some(host.clone())
    );
    let id = session.id().to_owned();
    soon(agent.close()).await.unwrap();

    let probe = Arc::new(Probe::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let writer = probe.clone();
    builder.require_feature(
        Feature::new("test.branch-probe").branch("test.branch-probe", move |names| {
            Ok(Box::new(ProbePlugin::new(names, writer.clone())?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.restore_session(id)).await.unwrap();
    assert_eq!(source(session.branches(), &child), Some(host));
    assert_eq!(source(session.branches(), "main"), None);
    assert_eq!(
        session
            .branches()
            .into_iter()
            .find(|info| info.id == child)
            .unwrap()
            .parent,
        Some("main".into())
    );
    soon(agent.close()).await.unwrap();
}

/// A data-method caller can leave while its committed child is still being
/// built. Its cancellation wait is call-owned and must release with the
/// caller; the tree still settles the accepted creation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_creation_releases_its_cancellation_wait_when_the_caller_leaves() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    pause(&session, &probe).await;
    let high = session.sequence("main").unwrap();
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    let entered = Arc::new(Notify::new());
    let released = tokio_util::sync::CancellationToken::new();
    let cancelled: Arc<
        dyn Fn() -> rutis::host::BoxFuture<'static, std::result::Result<(), Error>> + Send + Sync,
    > = Arc::new({
        let entered = entered.clone();
        let released = released.clone();
        move || {
            let entered = entered.clone();
            let release = released.clone().drop_guard();
            Box::pin(async move {
                let _release = release;
                entered.notify_one();
                std::future::pending().await
            })
        }
    });
    let mut waiting = Box::pin(commands.create("main".into(), high, options("child"), cancelled));
    soon(async {
        tokio::select! {
            _ = entered.notified() => {}
            result = &mut waiting => panic!("creation passed held dispatch: {result:?}"),
        }
    })
    .await;
    let create = wait_create(&session, high).await;
    drop(waiting);
    let released = tokio::time::timeout(Duration::from_secs(1), released.cancelled()).await;
    probe.release.notify_one();
    let child = soon(async {
        loop {
            if let Some(child) = session
                .branches()
                .into_iter()
                .find(|info| info.id != "main")
            {
                break child;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    soon(session.idle(&child.id)).await.unwrap();
    assert_eq!(child.create_anchor, Some(create.sequence));
    soon(agent.close()).await.unwrap();
    assert!(
        released.is_ok(),
        "the caller left its cancellation wait running"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_prompt_position_and_fixed_ancestor_prefix_drive_creation() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("ancestor".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let high = session.sequence("main").unwrap();
    let fixed = soon(session.branch("main".into(), high, options("fixed head")))
        .await
        .unwrap();
    soon(session.idle(&fixed)).await.unwrap();
    let fixed_create = create_event(&branch_events(&session, "main", ..).await.unwrap());
    assert!(matches!(&fixed_create.kind, EventKind::BranchCreate(data)
        if data.at == BranchCreateAt::Sequence { sequence: high }));
    assert_eq!(
        session
            .branches()
            .iter()
            .find(|info| info.id == fixed)
            .unwrap()
            .branch_point,
        Some(high),
        "a caller's head remains a fixed cut while the parent can advance"
    );
    *probe.rewrite.lock().unwrap() = Rewrite::Business(
        BranchCreateAt::Sequence { sequence: 1 },
        "final prompt".into(),
    );
    let child = soon(session.branch("main".into(), high, options("candidate")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let create = create_event(&branch_events(&session, "main", ..).await.unwrap());
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap();
    assert_eq!(
        (info.branch_point, info.create_anchor),
        (Some(1), Some(create.sequence))
    );
    let local = branch_events(&session, &child, ..).await.unwrap();
    assert!(
        matches!(&local[0].kind, EventKind::BranchStart(data) if data.create_event == Some(create.sequence))
    );
    let prompt = local
        .iter()
        .find(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .expect("the branch's first input");
    assert!(
        matches!(&prompt.kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == "final prompt"
        ))
    );
    let inherited = chain_events(&session, &child, ..=0).await.unwrap();
    assert_eq!(inherited.len(), 1);
    assert_eq!(
        inherited[0],
        branch_events(&session, "main", ..=1).await.unwrap()[0]
    );
    let local = branch_events(&session, &child, ..).await.unwrap();
    let applied: Vec<&str> = local
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputApplied(data) => Some(data.input_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        applied
            .iter()
            .filter(|id| **id == prompt.id.as_str())
            .count(),
        1,
        "the opening input is applied once"
    );
    for id in applied.iter().filter(|id| **id != prompt.id.as_str()) {
        let input = local
            .iter()
            .find(|event| event.id == **id)
            .expect("an applied input");
        assert!(
            matches!(&input.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|block| matches!(block, InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-parent")))),
            "the line's own turn applies the opening and the parent block only"
        );
    }
    assert!(
        chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| event.id == create.id)
    );
    *probe.rewrite.lock().unwrap() =
        Rewrite::Business(BranchCreateAt::Create {}, "tail prompt".into());
    let tail = soon(session.branch("main".into(), 0, options("ignored")))
        .await
        .unwrap();
    let tail_create = create_event(&branch_events(&session, "main", ..).await.unwrap());
    let tail_info = session
        .branches()
        .into_iter()
        .find(|info| info.id == tail)
        .unwrap();
    assert_eq!(tail_info.branch_point, Some(tail_create.sequence));
    assert_eq!(tail_info.create_anchor, Some(tail_create.sequence));
    assert_eq!(
        chain_events(&session, &tail, ..=0).await.unwrap(),
        chain_events(&session, "main", ..=tail_create.sequence)
            .await
            .unwrap()
    );
    soon(session.idle(&tail)).await.unwrap();
    *probe.rewrite.lock().unwrap() = Rewrite::None;
    let grandchild = soon(session.branch(tail.clone(), 0, options("grandchild")))
        .await
        .unwrap();
    soon(session.idle(&grandchild)).await.unwrap();
    assert_eq!(
        chain_events(&session, &grandchild, ..=0).await.unwrap(),
        chain_events(&session, &tail, ..=0).await.unwrap()
    );
    assert_eq!(
        probe.child_builds.load(Ordering::SeqCst),
        4,
        "inherited create events must not be dispatched"
    );
    soon(agent.close()).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_create_uses_its_anchor_and_survives_an_abandoned_waiter() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    pause(&session, &probe).await;
    let high = session.sequence("main").unwrap();
    let caller = session.clone();
    let admission = tokio_util::sync::CancellationToken::new();
    let token = admission.clone();
    let waiting = tokio::spawn(async move {
        caller
            .branch_with_admission("main".into(), high, options("child"), token)
            .await
    });
    let create = wait_create(&session, high).await;
    admission.cancel();
    assert!(matches!(
        soon(waiting).await.unwrap(),
        Err(AgentError::Cancelled(_))
    ));
    assert_eq!(probe.child_builds.load(Ordering::SeqCst), 0);
    let later = session
        .submit("later".into(), InputMode::Append, "main")
        .await
        .unwrap();
    probe.release.notify_one();
    soon(session.idle("main")).await.unwrap();
    let child = soon(async {
        loop {
            if let Some(child) = session
                .branches()
                .into_iter()
                .find(|info| info.id != "main")
            {
                return child;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    soon(session.idle(&child.id)).await.unwrap();
    assert_eq!(
        (child.branch_point, child.create_anchor),
        (Some(high), Some(create.sequence))
    );
    let prefix = chain_events(&session, &child.id, ..=0).await.unwrap();
    assert_eq!(prefix.last().unwrap().sequence, high);
    assert!(!prefix.iter().any(|event| event.id == create.id));
    assert!(!prefix.iter().any(|event| event.id == later.id));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_releases_an_unconsumed_create_without_waiting_for_dispatch() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    pause(&session, &probe).await;
    let high = session.sequence("main").unwrap();
    let caller = session.clone();
    let waiting =
        tokio::spawn(async move { caller.branch("main".into(), high, options("child")).await });
    wait_create(&session, high).await;
    let close = session.close();
    assert!(matches!(
        soon(waiting).await.unwrap(),
        Err(AgentError::Cancelled(_))
    ));
    assert_eq!(probe.child_builds.load(Ordering::SeqCst), 0);
    probe.release.notify_one();
    soon(close).await.unwrap();
    let failures: Vec<_> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreateFailed(data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 1);
    assert!(matches!(
        &failures[0],
        BranchCreateFailedData::Cancelled { reason, .. }
            if reason == "branch creation cancelled"
    ));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_close_waits_for_dispatch_before_releasing_branch_reservations() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    pause(&session, &probe).await;
    let main = probe
        .commands
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get("main")
        .unwrap();
    let high = session.sequence("main").unwrap();
    let caller = session.clone();
    let waiting =
        tokio::spawn(async move { caller.branch("main".into(), high, options("child")).await });
    let create = wait_create(&session, high).await;
    let mut events = Committed::after(&session, "main", create.sequence);
    let mut closing = main.close();
    soon(async {
        loop {
            if matches!(events.next().await.kind, EventKind::StopInterrupt(_)) {
                break;
            }
        }
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut closing)
            .await
            .is_err()
    );
    assert!(!waiting.is_finished());
    assert_eq!(probe.child_builds.load(Ordering::SeqCst), 0);
    probe.release.notify_one();
    let child = soon(waiting).await.unwrap().unwrap();
    soon(closing).await.unwrap();
    assert!(main.state().unwrap().closed);
    assert!(main.state().unwrap().failure.is_none());
    assert_eq!(session.branches().len(), 2);
    session
        .submit("still usable".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    assert!(
        chain_events(&session, &child, ..)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(&event.kind, EventKind::Text(data) if data.delta.contains("still usable"))
            })
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_build_rolls_back_and_releases_capacity_even_without_its_waiter() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    probe.fail_child.store(true, Ordering::SeqCst);
    let internal = soon(session.branch("main".into(), 0, options("rejected")))
        .await
        .unwrap_err();
    assert!(internal.to_string().contains("/private/agent-secret"));
    assert_eq!(probe.child_cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(*probe.cleanup_write.lock().unwrap(), Some(true));
    assert_eq!(session.branches().len(), 1);
    let failures: Vec<_> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreateFailed(data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 1);
    assert!(matches!(
        &failures[0],
        BranchCreateFailedData::Failed { error, .. }
            if error == "branch creation failed" && !error.contains("/private")
    ));
    session
        .submit("still usable".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    probe.fail_child.store(false, Ordering::SeqCst);
    probe.child_builds.store(0, Ordering::SeqCst);
    probe.child_cleanups.store(0, Ordering::SeqCst);
    let rejected = String::new();
    assert!(
        soon(session.branch("main".into(), 0, options(&rejected)))
            .await
            .is_err()
    );
    assert_eq!(session.branches().len(), 1);
    assert_eq!(probe.child_cleanups.load(Ordering::SeqCst), 1);
    probe.delay_child.store(true, Ordering::SeqCst);
    let caller = session.clone();
    let waiting =
        tokio::spawn(async move { caller.branch("main".into(), 0, options(&rejected)).await });
    soon(async {
        while probe.child_builds.load(Ordering::SeqCst) < 2 {
            probe.child_entered.notified().await;
        }
    })
    .await;
    waiting.abort();
    let _ = waiting.await;
    probe.child_release.notify_one();
    soon(async {
        while probe.child_cleanups.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    probe.delay_child.store(false, Ordering::SeqCst);
    let child = soon(session.branch("main".into(), 0, options("works")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidates_remain_private_until_the_initial_input_is_accepted() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    *probe.session.lock().unwrap() = Some(session.clone());
    let rejected = String::new();
    assert!(
        soon(session.branch("main".into(), 0, options(&rejected)))
            .await
            .is_err()
    );
    assert_eq!(session.branches().len(), 1);
    *probe.rewrite.lock().unwrap() = Rewrite::Identity("transformed-child".into());
    let child = soon(session.branch("main".into(), 0, options("accepted")))
        .await
        .unwrap();
    assert_eq!(child, "transformed-child");
    let checks = probe.publication_checks.lock().unwrap().clone();
    assert!(
        checks.len() >= 3,
        "both branch/start events and accepted input were observed"
    );
    assert!(
        checks.iter().all(|check| *check == (false, false)),
        "candidate was public: {checks:?}"
    );
    assert!(session.branches().iter().any(|info| info.id == child));
    assert!(
        branch_events(&session, &child, ..)
            .await
            .unwrap()
            .iter()
            .any(
                |event| matches!(&event.kind, EventKind::InputMessage(data) if matches!(
                    data.user.as_ref().map(|user| user.content.as_slice()),
                    Some([ContentBlock::Text { text }]) if text == "accepted"
                ))
            )
    );
    let before = session.sequence("main").unwrap();
    assert!(matches!(
        soon(session.branch("main".into(), 0, options("collision"))).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert_eq!(session.sequence("main").unwrap(), before);
    assert_eq!(session.branches().len(), 2);
    probe.session.lock().unwrap().take();
    soon(agent.close()).await.unwrap();
}

/// The loop covers both direct user input and a trusted hook selecting the
/// same unusable cut.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_batch_cut_settles_creation_without_faulting_the_session() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let timeline = probe.main_timeline.lock().unwrap().clone().unwrap();
    let batch = timeline
        .append_batch_registered(vec![interrupt().into(), interrupt().into()], |_| Ok(()))
        .unwrap();
    for rewrite in [false, true] {
        if rewrite {
            *probe.rewrite.lock().unwrap() = Rewrite::Business(
                BranchCreateAt::Sequence {
                    sequence: batch[0].sequence,
                },
                "rewritten".into(),
            );
        }
        let at = if rewrite {
            batch[1].sequence
        } else {
            batch[0].sequence
        };
        let error = soon(session.branch("main".into(), at, options("invalid")))
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::InvalidInput(ref message) if message.contains("inside an atomic batch")),
            "{error:?}"
        );
        assert_eq!(session.branches().len(), 1);
        assert_eq!(probe.child_builds.load(Ordering::SeqCst), 0);
        session
            .submit("still usable".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        soon(session.idle("main")).await.unwrap();
    }
    *probe.rewrite.lock().unwrap() = Rewrite::None;
    let child = soon(session.branch("main".into(), batch[1].sequence, options("valid")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_output_cannot_be_inherited_as_an_asymmetric_prefix() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("complete model response".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = branch_events(&session, "main", ..).await.unwrap();
    let model_start = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ModelStart(_)))
        .unwrap();
    let model_end = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ModelOutputEnd(_)))
        .unwrap();

    let error = soon(session.branch(
        "main".into(),
        model_start.sequence,
        options("invalid prefix"),
    ))
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        AgentError::InvalidInput(message) if message.contains("unfinished model output")
    ));
    assert_eq!(session.branches().len(), 1);

    let child = soon(session.branch(
        "main".into(),
        model_end.sequence,
        options("complete prefix"),
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_on_main_opens_a_line_before_its_own_request() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder(&probe)).await.unwrap();
    let root = agent.root().clone();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let broadcast = Broadcast::listen(&root, session.id());
    session
        .submit(
            r#"tool:branch {"action":"create","prompt":"child"}"#.into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let branches = session.branches();
    let child = branches
        .iter()
        .find(|info| info.parent.as_deref() == Some("main"))
        .expect("the tool call opened a line");
    soon(session.idle(&child.id)).await.unwrap();
    let main_events = branch_events(&session, "main", ..).await.unwrap();
    let model_start = main_events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ModelStart(_)))
        .unwrap();
    assert_eq!(child.branch_point, Some(model_start.sequence - 1));
    let anchor = create_event(&main_events);
    assert_eq!(child.create_anchor, Some(anchor.sequence));
    assert!(
        main_events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ToolResult(data)
        if data.results[0].result.is_error != Some(true)
            && matches!(&data.results[0].result.content,
                ToolContent::Text(text) if text == &format!("Line created: {}", child.id))))
    );
    let mut open_last = std::collections::HashMap::<String, u64>::new();
    for info in &branches {
        let local = branch_events(&session, &info.id, ..).await.unwrap();
        open_last.insert(info.id.clone(), local.last().unwrap().sequence);
    }
    soon(agent.close()).await.unwrap();
    let records = soon(broadcast.ended()).await;
    let (end, events) = records.split_last().unwrap();
    assert!(end.is_none());
    let mut ids = std::collections::HashSet::new();
    let mut last = std::collections::HashMap::<String, u64>::new();
    for (branch, event) in events.iter().map(|record| record.as_ref().unwrap()) {
        assert!(ids.insert(event.id.clone()), "events are broadcast once");
        let local = branch_events(&session, branch, ..).await.unwrap();
        assert!(Arc::ptr_eq(&local[event.sequence as usize - 1], event));
        let previous = last.insert(branch.clone(), event.sequence);
        if let Some(previous) = previous {
            assert_eq!(event.sequence, previous + 1, "branch order is kept");
        }
    }
    for info in &branches {
        assert!(
            last.get(&info.id).copied().unwrap_or(0) >= open_last[&info.id],
            "the broadcast delivered every event the open session committed; a turn the close's own stop starts need not reach it"
        );
    }
}

async fn tool_call(session: &Session, branch: &str, arguments: &str) -> ToolResult {
    let before = tool_results(session, branch).await.len();
    session
        .submit(format!("tool:branch {arguments}"), InputMode::Queue, branch)
        .await
        .unwrap();
    soon(session.idle(branch)).await.unwrap();
    soon(async {
        loop {
            if let Some(result) = tool_results(session, branch)
                .await
                .into_iter()
                .skip(before)
                .last()
            {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

async fn tool_results(session: &Session, branch: &str) -> Vec<ToolResult> {
    local_events(session, branch)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .collect()
}
async fn call(session: &Session, branch: &str, arguments: &str) -> (String, String) {
    let result = tool_call(session, branch, arguments).await;
    assert_eq!(result.is_error, Some(true), "{:?}", result.content);
    let structured = result.structured_content.clone().unwrap_or_default();
    (
        structured
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        structured
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    )
}
/// The events of one line, whole batches, the way a host pages a line: the tree
/// serves both an open line and one that ended.
async fn line_events(session: &Session, branch: &str) -> Result<Vec<Arc<AgentEvent>>> {
    session
        .session_service::<TreeCommands>()?
        .page(branch.to_owned(), 0, LINE_PAGE, session.sequence(branch)?)
        .await
}
async fn local_events(session: &Session, branch: &str) -> Vec<Arc<AgentEvent>> {
    line_events(session, branch).await.unwrap()
}
async fn until_events(
    session: &Session,
    branch: &str,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    for _ in 0..600 {
        if let Ok(events) = line_events(session, branch).await
            && done(&events)
        {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for line {branch}");
}
fn line_block(events: &[Arc<AgentEvent>]) -> Option<(String, bool)> {
    keyed_block(events, "branch:line")
}
/// The latest block under the roster's own key, `branch:roster`.
fn roster_block(events: &[Arc<AgentEvent>]) -> Option<(String, bool)> {
    keyed_block(events, "branch:roster")
}
fn keyed_block(events: &[Arc<AgentEvent>], keyed: &str) -> Option<(String, bool)> {
    keyed_blocks(events, keyed).last().cloned()
}
/// Every block under `keyed`, in commit order, with its retire flag.
fn keyed_blocks(events: &[Arc<AgentEvent>], keyed: &str) -> Vec<(String, bool)> {
    events
        .iter()
        .flat_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data
                .blocks
                .iter()
                .filter_map(|block| match block {
                    InputBlock::Raw {
                        text,
                        key: Some(key),
                        retire,
                    } if key == keyed => Some((text.clone(), *retire)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}
fn blocks_with(events: &[Arc<AgentEvent>], marker: &str) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data
                .blocks
                .iter()
                .filter_map(|block| match block {
                    InputBlock::Raw { text, .. } if text.starts_with(marker) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}
/// The page a `branch` read returned.
fn page_of(result: &ToolResult) -> serde_json::Value {
    assert_eq!(result.is_error, None, "{:?}", result.content);
    serde_json::Value::Object(
        result
            .structured_content
            .clone()
            .expect("a read page is structured"),
    )
}
fn page_sequences(page: &serde_json::Value) -> Vec<u64> {
    page["items"]
        .as_array()
        .expect("page items")
        .iter()
        .filter_map(|item| item.get("sequence").and_then(serde_json::Value::as_u64))
        .collect()
}
/// The `head` and `after` a `<dim-branch-parent>` block reports.
fn parent_block(text: &str) -> (u64, u64) {
    let attr = |name: &str| {
        let start = text.find(&format!("{name}=\"")).expect("attribute") + name.len() + 2;
        let rest = &text[start..];
        rest[..rest.find('"').expect("attribute end")]
            .parse()
            .expect("attribute value")
    };
    (attr("head"), attr("after"))
}
/// The line a `<dim-branch-parent>` block names.
fn parent_block_line(text: &str) -> String {
    let start = text.find("branch=\"").expect("the block names its branch") + "branch=\"".len();
    let rest = &text[start..];
    rest[..rest.find('"').expect("the attribute ends")].to_string()
}
/// The parent block inputs on a line, by event sequence.
fn parent_block_events(events: &[Arc<AgentEvent>]) -> Vec<(u64, String)> {
    events
        .iter()
        .filter_map(|event| {
            let text = blocks_with(std::slice::from_ref(event), "<dim-branch-parent")
                .into_iter()
                .next()?;
            Some((event.sequence, text))
        })
        .collect()
}
fn block_held(events: &[Arc<AgentEvent>], sequence: u64) -> bool {
    let Some(block) = events.iter().find(|event| event.sequence == sequence) else {
        return false;
    };
    !events.iter().any(
        |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == block.id),
    )
}
/// Waits for a restored Session to publish one of its branches.
async fn until_branch(session: &Session, branch: &str) {
    for _ in 0..600 {
        if session.sequence(branch).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("branch {branch} was not restored");
}
/// Waits for a held parent block covering the parent's latest operator input
/// or tool use. Returns its sequence, text and the parent's observed head.
async fn settle_line(session: &Session, branch: &str) -> (u64, String, u64) {
    for _ in 0..400 {
        patient(session.idle("main")).await.unwrap();
        patient(session.idle(branch)).await.unwrap();
        let head = session.sequence("main").unwrap();
        let events = local_events(session, branch).await;
        let Some((sequence, text)) = parent_block_events(&events).last().cloned() else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        if !block_held(&events, sequence) {
            // A consumed block at the head means the turn that consumed it is
            // waking the parent; wait for that turn to land.
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }
        let (block_head, _) = parent_block(&text);
        let activity = local_events(session, "main")
            .await
            .iter()
            .filter(|event| {
                matches!(&event.kind, EventKind::InputMessage(data) if data.user.is_some())
                    || matches!(event.kind, EventKind::ToolUse(_))
            })
            .map(|event| event.sequence)
            .max()
            .unwrap_or(0);
        if block_head < activity {
            patient(session.submit("settle".into(), InputMode::Queue, branch))
                .await
                .unwrap();
            patient(session.idle(branch)).await.unwrap();
            continue;
        }
        // Held at the head: an in-flight wake of the parent would show itself
        // as a new head and block within this window.
        tokio::time::sleep(Duration::from_millis(20)).await;
        if session.sequence("main").unwrap() != head {
            continue;
        }
        let events = local_events(session, branch).await;
        if parent_block_events(&events)
            .last()
            .is_some_and(|(latest, _)| *latest == sequence)
            && block_held(&events, sequence)
        {
            return (sequence, text, head);
        }
    }
    panic!("line {branch} did not settle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_branch_call_writes_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let host = soon(session.branch("main".into(), 0, options("host line")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    session
        .submit(
            r#"tool:branch {"action":"create","prompt":"tool line"}"#.into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let tool_line = session
        .branches()
        .iter()
        .find(|info| info.parent.as_deref() == Some("main"))
        .expect("main opened a tool line")
        .id
        .clone();
    soon(session.idle(&tool_line)).await.unwrap();
    assert_eq!(
        call(
            &session,
            &tool_line,
            r#"{"action":"create","prompt":"grandchild"}"#
        )
        .await,
        (
            "branch_not_opener".into(),
            "Only the root and the default branch open lines. Tell one of them if more parallel work is needed."
                .into()
        )
    );
    assert!(
        !session
            .branches()
            .iter()
            .any(|info| info.parent.as_deref() == Some(tool_line.as_str()))
    );
    let before = branch_events(&session, &host, ..).await.unwrap().len();
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"message","to":"{host}","content":"hello"}}"#)
        )
        .await,
        (
            "branch_not_tool_line".into(),
            format!("Line {host} was not opened with this tool; it follows its own protocol.")
        )
    );
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{host}"}}"#)
        )
        .await,
        (
            "branch_not_tool_line".into(),
            format!("Line {host} was not opened with this tool; it follows its own protocol.")
        )
    );
    // A refused call writes no message to its target; the runtime may still
    // write its own parent block on an open line.
    for event in branch_events(&session, &host, ..)
        .await
        .unwrap()
        .iter()
        .skip(before)
    {
        if let EventKind::InputMessage(data) = &event.kind {
            assert!(
                !data.blocks.is_empty()
                    && data.blocks.iter().all(|block| matches!(
                        block,
                        InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-parent")
                    )),
                "a refused call writes nothing to {host}: {:?}",
                event.kind
            );
        }
    }
    let main_before = branch_events(&session, "main", ..).await.unwrap().len();
    assert_eq!(
        call(
            &session,
            &tool_line,
            &format!(r#"{{"action":"read","branch":"{host}"}}"#)
        )
        .await,
        (
            "branch_not_kin".into(),
            "You can message or read only the lines you opened and the line you were cut from."
                .into()
        )
    );
    assert_eq!(
        call(
            &session,
            &host,
            &format!(r#"{{"action":"read","branch":"{tool_line}"}}"#)
        )
        .await,
        (
            "branch_not_kin".into(),
            "You can message or read only the lines you opened and the line you were cut from."
                .into()
        )
    );
    // A refused call writes no message to its target; the runtime may still
    // write its own parent block on an open line.
    for line in [&host, &tool_line] {
        let events = branch_events(&session, line, ..).await.unwrap();
        for event in &events {
            if let EventKind::InputMessage(data) = &event.kind {
                assert!(
                    !data.blocks.iter().any(|block| matches!(
                        block,
                        InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-message")
                    )),
                    "a refused call writes no message to {line}"
                );
            }
        }
    }
    let notices = ["<dim-branch-idle", "<dim-branch-lines", "<dim-branch-ended"];
    for event in branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .skip(main_before)
    {
        if matches!(event.kind, EventKind::InputMessage(_)) {
            assert!(
                notices
                    .iter()
                    .any(|prefix| !blocks_with(std::slice::from_ref(event), prefix).is_empty()),
                "a refused read writes nothing to main: {:?}",
                event.kind
            );
        }
    }
    assert_eq!(
        call(
            &session,
            &tool_line,
            &format!(r#"{{"action":"message","to":"{host}","content":"hello"}}"#)
        )
        .await,
        (
            "branch_not_kin".into(),
            "You can message or read only the lines you opened and the line you were cut from."
                .into()
        )
    );
    assert_eq!(
        call(&session, "main", r#"{"action":"end","content":"done"}"#).await,
        (
            "branch_cannot_end".into(),
            "Only a line opened with create can end itself, and not while it or a line below it is the default branch.".into()
        )
    );
    assert_eq!(
        call(&session, &host, r#"{"action":"end","content":"done"}"#).await,
        (
            "branch_cannot_end".into(),
            "Only a line opened with create can end itself, and not while it or a line below it is the default branch.".into()
        )
    );
    assert!(
        !branch_events(&session, &host, ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_)))
    );
    assert!(
        !branch_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_)))
    );
    soon(agent.close()).await.unwrap();
}

/// The reach of the tool: a line messages and reads the line it was cut from
/// and each tool line it opened, while a line the user opened is on no roster
/// and under no coordination of the line it was cut from, which neither
/// messages nor reads it (design §6, §20.1, §20.3a).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_messages_and_reads_its_parent_and_the_lines_it_opened() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let tool_line = open_line(&session, "opened with the tool").await;
    let page = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"read","branch":"{tool_line}"}}"#),
    )
    .await;
    assert_eq!(
        page.is_error, None,
        "the opener reads the line it opened: {:?}",
        page.content
    );
    let sent = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"message","to":"{tool_line}","content":"FROM-THE-OPENER"}}"#),
    )
    .await;
    assert_eq!(
        sent.is_error, None,
        "the opener messages it: {:?}",
        sent.content
    );
    let sent = tool_call(
        &session,
        &tool_line,
        r#"{"action":"message","to":"main","content":"FROM-THE-LINE"}"#,
    )
    .await;
    assert_eq!(
        sent.is_error, None,
        "a line messages the line it was cut from: {:?}",
        sent.content
    );
    let page =
        page_of(&tool_call(&session, &tool_line, r#"{"action":"read","branch":"main"}"#).await);
    assert!(
        !page_sequences(&page).is_empty(),
        "a line reads the line it was cut from: {page}"
    );

    // The user opens a line of its own at the root's head, which must end a
    // committed batch rather than sit inside the root's answer to the line.
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let user = soon(session.branch(
        "main".into(),
        cut,
        BranchOptions {
            prompt: "the user opened this".into(),
            source: BranchSource::User {
                operation_id: "client-op".into(),
            },
        },
    ))
    .await
    .unwrap();
    quiet(&session, &user).await;
    let roster = blocks_with(&local_events(&session, "main").await, "<dim-branch-lines")
        .last()
        .cloned()
        .expect("the root's roster");
    assert!(roster.contains(&tool_line), "{roster}");
    assert!(
        !roster.contains(&user),
        "a line the user opened is on no roster: {roster}"
    );
    let not_kin = (
        "branch_not_kin".to_string(),
        "You can message or read only the lines you opened and the line you were cut from."
            .to_string(),
    );
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"message","to":"{user}","content":"hello"}}"#)
        )
        .await,
        not_kin,
        "the line it was cut from sends it no message"
    );
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{user}"}}"#)
        )
        .await,
        not_kin,
        "the line it was cut from reads it no more"
    );
    let idle_notices = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    let ended_notices =
        blocks_with(&local_events(&session, "main").await, "<dim-branch-ended").len();
    soon(session.submit("WORK-ON".into(), InputMode::Queue, &user))
        .await
        .unwrap();
    quiet(&session, &user).await;
    assert_eq!(
        blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len(),
        idle_notices,
        "the user's line tells the line it was cut from nothing"
    );
    assert_eq!(
        blocks_with(&local_events(&session, "main").await, "<dim-branch-ended").len(),
        ended_notices,
        "and no ended notice either"
    );
    // A line the user opened messages the line it was cut from in both modes:
    // an append rides its next request and starts no turn, a steer wakes it
    // (design §6, §20.2).
    let before = session.sequence("main").unwrap();
    let sent = tool_call(
        &session,
        &user,
        r#"{"action":"message","to":"main","content":"APPEND-FROM-THE-USERS-LINE"}"#,
    )
    .await;
    assert_eq!(
        sent.is_error, None,
        "a line the user opened messages the line it was cut from: {:?}",
        sent.content
    );
    let events = until_events(&session, "main", |events| {
        message_at(events, "APPEND-FROM-THE-USERS-LINE").is_some()
    })
    .await;
    let (appended, mode) =
        message_at(&events, "APPEND-FROM-THE-USERS-LINE").expect("the append landed on main");
    assert_eq!(mode, InputMode::Append);
    assert!(appended > before);
    let block = marked_message(&events, "APPEND-FROM-THE-USERS-LINE");
    assert!(
        block.starts_with(&format!("<dim-branch-message from=\"{user}\" to=\"main\">")),
        "the receiver tells the message from the user's words by its marker: {block}"
    );
    let started = events
        .iter()
        .any(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > appended);
    assert!(
        !started,
        "an append from a line the user opened starts no turn on the line it was cut from"
    );
    let sent = tool_call(
        &session,
        &user,
        r#"{"action":"message","to":"main","content":"STEER-FROM-THE-USERS-LINE","mode":"steer"}"#,
    )
    .await;
    assert_eq!(sent.is_error, None, "{:?}", sent.content);
    let events = until_events(&session, "main", |events| {
        message_at(events, "STEER-FROM-THE-USERS-LINE").is_some()
    })
    .await;
    let (steered, mode) =
        message_at(&events, "STEER-FROM-THE-USERS-LINE").expect("the steer landed on main");
    assert_eq!(mode, InputMode::Steer);
    let block = marked_message(&events, "STEER-FROM-THE-USERS-LINE");
    assert!(
        block.starts_with(&format!("<dim-branch-message from=\"{user}\" to=\"main\">")),
        "the steer carries the marker too: {block}"
    );
    until_events(&session, "main", |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > steered)
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    let page = page_of(&tool_call(&session, &user, r#"{"action":"read","branch":"main"}"#).await);
    let sequences = page_sequences(&page);
    assert!(!sequences.is_empty(), "{page}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cut),
        "a read of the line it was cut from starts at its cut {cut}: {sequences:?}"
    );

    // A plugin line's protocol is its creator's: it is never on a roster, and
    // the line that opened it reaches it only to be told so (design §6).
    let plugin = soon(session.branch(tool_line.clone(), 0, options("a plugin line")))
        .await
        .unwrap();
    quiet(&session, &plugin).await;
    assert!(
        roster_block(&local_events(&session, &tool_line).await).is_none(),
        "a line that opened no tool line keeps no roster"
    );
    let not_tool_line = (
        "branch_not_tool_line".to_string(),
        format!("Line {plugin} was not opened with this tool; it follows its own protocol."),
    );
    assert_eq!(
        call(
            &session,
            &tool_line,
            &format!(r#"{{"action":"read","branch":"{plugin}"}}"#)
        )
        .await,
        not_tool_line.clone(),
        "a line does not read a plugin line it opened"
    );
    assert_eq!(
        call(
            &session,
            &tool_line,
            &format!(r#"{{"action":"message","to":"{plugin}","content":"hello"}}"#)
        )
        .await,
        not_tool_line,
        "nor message it"
    );
    soon(agent.close()).await.unwrap();
}

/// A plugin line coordinates nothing through the tool, in either direction: it
/// sends the line it was cut from no message, and the refusal names the caller
/// as the line that was not opened with this tool, while a line's read of its
/// opener still works (design §6, §20.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_sends_the_line_it_was_cut_from_no_message() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let host = soon(session.branch("main".into(), 0, options("a plugin line")))
        .await
        .unwrap();
    quiet(&session, &host).await;
    let not_tool_line = (
        "branch_not_tool_line".to_string(),
        format!("Line {host} was not opened with this tool; it follows its own protocol."),
    );
    let written = blocks_with(&local_events(&session, "main").await, "<dim-branch-message").len();
    assert_eq!(
        call(
            &session,
            &host,
            r#"{"action":"message","to":"main","content":"hello"}"#
        )
        .await,
        not_tool_line.clone(),
        "a plugin line sends its opener no message, and the refusal names the caller"
    );
    assert_eq!(
        blocks_with(&local_events(&session, "main").await, "<dim-branch-message").len(),
        written,
        "the refused message writes nothing to the line it was cut from"
    );
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"message","to":"{host}","content":"hello"}}"#)
        )
        .await,
        not_tool_line,
        "and the opener sends it none either"
    );
    let page = page_of(&tool_call(&session, &host, r#"{"action":"read","branch":"main"}"#).await);
    assert!(
        !page_sequences(&page).is_empty(),
        "a plugin line still reads the line it was cut from: {page}"
    );
    soon(agent.close()).await.unwrap();
}

/// The message block carrying `content`, as its receiver stored it.
fn marked_message(events: &[Arc<AgentEvent>], content: &str) -> String {
    blocks_with(events, "<dim-branch-message")
        .into_iter()
        .find(|text| text.contains(content))
        .expect("the message block")
}

/// The sequence of the input carrying the message `content`, and its mode.
fn message_at(events: &[Arc<AgentEvent>], content: &str) -> Option<(u64, InputMode)> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::InputMessage(data)
            if blocks_with(std::slice::from_ref(event), "<dim-branch-message")
                .iter()
                .any(|text| text.contains(content)) =>
        {
            Some((event.sequence, data.effective_mode()))
        }
        _ => None,
    })
}

/// How many message blocks each of `lines` holds in its own history.
async fn messages_written(session: &Session, lines: &[&String]) -> Vec<usize> {
    let mut counts = Vec::new();
    for line in lines {
        counts.push(blocks_with(&local_events(session, line).await, "<dim-branch-message").len());
    }
    counts
}

/// Siblings and a line's grandparent are refused `branch_not_kin` and write
/// nothing: the pair the tool serves is one edge of the tree (design §6,
/// §20.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sibling_or_grandparent_is_refused_with_nothing_written() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let sibling = |prompt: &'static str| {
        let session = session.clone();
        async move {
            soon(session.branch(
                "main".into(),
                0,
                BranchOptions {
                    prompt: prompt.into(),
                    source: BranchSource::User {
                        operation_id: format!("user:{prompt}"),
                    },
                },
            ))
            .await
            .unwrap()
        }
    };
    let first = sibling("first").await;
    let second = sibling("second").await;
    quiet(&session, &first).await;
    quiet(&session, &second).await;
    let grandchild = soon(session.branch(
        first.clone(),
        0,
        BranchOptions {
            prompt: "the child of the first line".into(),
            source: BranchSource::User {
                operation_id: "user:grandchild".into(),
            },
        },
    ))
    .await
    .unwrap();
    quiet(&session, &grandchild).await;
    let lines = [&first, &second, &grandchild];
    let before = messages_written(&session, &lines).await;
    let not_kin = (
        "branch_not_kin".to_string(),
        "You can message or read only the lines you opened and the line you were cut from."
            .to_string(),
    );
    assert_eq!(
        call(
            &session,
            &first,
            &format!(r#"{{"action":"message","to":"{second}","content":"hello"}}"#)
        )
        .await,
        not_kin.clone(),
        "a sibling gets no message"
    );
    assert_eq!(
        call(
            &session,
            &first,
            &format!(r#"{{"action":"read","branch":"{second}"}}"#)
        )
        .await,
        not_kin.clone(),
        "and no read"
    );
    assert_eq!(
        call(
            &session,
            &grandchild,
            r#"{"action":"read","branch":"main"}"#
        )
        .await,
        not_kin.clone(),
        "a line does not read its grandparent"
    );
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{grandchild}"}}"#)
        )
        .await,
        not_kin.clone(),
        "and the grandparent does not read it"
    );
    assert_eq!(
        messages_written(&session, &lines).await,
        before,
        "a refused call writes no message to any of the three lines"
    );
    let page = page_of(
        &tool_call(
            &session,
            &grandchild,
            &format!(r#"{{"action":"read","branch":"{first}"}}"#),
        )
        .await,
    );
    assert!(
        !page_sequences(&page).is_empty(),
        "a line still reads the line it was cut from: {page}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_cancels_an_active_build_and_waits_for_rollback() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    probe.delay_child.store(true, Ordering::SeqCst);
    let caller = session.clone();
    let waiting =
        tokio::spawn(async move { caller.branch("main".into(), 0, options("child")).await });
    soon(probe.child_entered.notified()).await;
    assert_eq!(session.branches().len(), 1, "a candidate is not listed");
    soon(session.close()).await.unwrap();
    assert!(soon(waiting).await.unwrap().is_err());
    assert_eq!(probe.child_cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(session.branches().len(), 1);
    soon(agent.close()).await.unwrap();
}

async fn open_line(session: &Session, purpose: &str) -> String {
    open_line_from(session, "main", purpose).await
}

/// Opens a tool line from `opener`, which must be the root or the current
/// default branch, and waits until it is idle.
async fn open_line_from(session: &Session, opener: &str, purpose: &str) -> String {
    let before = line_ids(session);
    let call = serde_json::json!({"action": "create", "prompt": purpose}).to_string();
    let result = tool_call(session, opener, &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    wait_new_line(session, &before).await
}

/// The open lines' ids at this moment.
fn line_ids(session: &Session) -> std::collections::BTreeSet<String> {
    session
        .branches()
        .iter()
        .map(|info| info.id.clone())
        .collect()
}

/// Waits until a line outside `before` is listed and idle, and answers its id.
async fn wait_new_line(session: &Session, before: &std::collections::BTreeSet<String>) -> String {
    let child = soon(async {
        loop {
            let opened = session
                .branches()
                .into_iter()
                .map(|info| info.id)
                .find(|id| !before.contains(id));
            if let Some(id) = opened {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    soon(session.idle(&child)).await.unwrap();
    child
}

/// The operator tells `main` something and its turn runs, so the line's next
/// parent turn end is activity it must hear about.
async fn move_main(session: &Session) {
    soon(session.submit("MOVE-ON".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
}
/// Waits until the line's purpose turn, and the parent's answers to what the
/// line wrote, have landed. A parent turn that only reads a line's block is not
/// activity, so a later movement must be the only one left.
async fn quiet(session: &Session, line: &str) {
    let mut last = (u64::MAX, u64::MAX);
    let mut stable = 0;
    for _ in 0..400 {
        patient(session.idle(line)).await.unwrap();
        patient(session.idle("main")).await.unwrap();
        let now = (
            session.sequence(line).unwrap(),
            session.sequence("main").unwrap(),
        );
        if now == last {
            stable += 1;
            if stable == 3 {
                return;
            }
        } else {
            stable = 0;
        }
        last = now;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("line {line} did not quiet");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_root_lists_its_open_tool_lines_under_the_roster_key() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let first = open_line(&session, "first purpose").await;
    let second = open_line(&session, "second purpose\nmore detail").await;
    let events = until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-lines")
            .last()
            .is_some_and(|text| text.contains(&second))
    })
    .await;
    assert!(
        line_block(&events).is_none(),
        "the roster is not the line's identity: {:?}",
        line_block(&events)
    );
    let (text, retired) = roster_block(&events).expect("the roster is keyed `branch:roster`");
    assert!(!retired, "the live roster is not retired");
    assert!(text.starts_with("<dim-branch-lines branch=\"main\">\n"));
    assert!(text.contains(&format!("\n- {first}: first purpose\n")));
    assert!(text.contains(&format!("\n- {second}: second purpose\n")));
    assert!(!text.contains("more detail"));
    assert!(text.ends_with("</dim-branch-lines>"));
    assert!(
        text.contains(
            "\nThey message you when they find something, need you, or end. Use the branch \
             tool to message or read them, and the `wait` tool to wait for them; end the wait \
             when your lines have nothing left to tell you.\n"
        ),
        "the roster carries the built-in closing sentence: {text}"
    );
    let history = chain_events(&session, &first, ..).await.unwrap();
    let (identity, retired) = line_block(&history).unwrap();
    assert!(!retired);
    assert!(identity.ends_with("</dim-branch-line>"));
    let facts = identity
        .split("\n\nHow you work with main:")
        .next()
        .expect("the factual sentences come before the rules");
    assert_eq!(
        facts,
        format!(
            "<dim-branch-line branch=\"{first}\" parent=\"main\" source=\"tool\">\n\
             You are line {first} of this session. Line main opened you at this point of its history to work in parallel with it. You are the same agent as main: everything above is your own history, and you continue from here on your own line. main keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to main, such as its list of open lines, are main's, not yours.\n\n\
             Your purpose, as main wrote it:\nfirst purpose"
        )
    );
    soon(agent.close()).await.unwrap();
}

/// The roster's closing sentence is the factory member
/// `BranchToolConfig::roster_closing`, and a configured value reaches the
/// roster byte for byte in place of the built-in sentence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_roster_closing_reaches_the_roster() {
    let closing = "Configured: report once and end your wait when nothing is left to tell.";
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(
        &probe,
        BranchToolConfig {
            roster_closing: Some(closing.into()),
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    let events = until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-lines")
            .last()
            .is_some_and(|text| text.contains(&child))
    })
    .await;
    let (text, retired) = roster_block(&events).expect("the roster is keyed `branch:roster`");
    assert!(!retired);
    assert!(
        text.ends_with(&format!("\n{closing}\n</dim-branch-lines>")),
        "{text}"
    );
    assert!(
        !text.contains("They message you when they find something"),
        "the built-in closing sentence is replaced: {text}"
    );
    soon(agent.close()).await.unwrap();
}

/// A note's `How you work here:` block is the factory member
/// `BranchToolConfig::default_note`: a configured value reaches the note byte
/// for byte, with the line ids its `{parent}` and `{other}` placeholders name,
/// while the note's protocol sentences stay the code's (design §12.1, §12.4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_note_block_reaches_the_note() {
    let block = "- Configured guidance for {parent}, switched from {other}.";
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(
        &probe,
        BranchToolConfig {
            default_note: Some(block.into()),
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "the configured line").await;
    switch(&session, &line).await;
    let notes = owed_notes(&session, &line, 1).await;
    let note = notes.last().expect("the line holds its note");
    assert!(
        note.contains("How you work here:\n- Configured guidance for main, switched from main."),
        "{note}"
    );
    assert!(!note.contains('{'), "no placeholder is left: {note}");
    assert!(
        note.contains("you send no automatic idle notice to main"),
        "the note's protocol sentences stay: {note}"
    );
    soon(agent.close()).await.unwrap();
}

/// The note a move writes on a tool line that becomes the default branch: its
/// birth identity, then the sentences the move changes, and no `How you work
/// here:` block, because `BranchToolConfig` sets no note member. `{line}` and
/// `{purpose}` stand where the code substitutes them.
const PROTOCOL_ONLY_TOOL_NOTE: &str = r#"<dim-branch-line branch="{line}" parent="main" source="tool" default="true">
You are line {line} of this session. Line main opened you at this point of its history to work in parallel with it. You are the same agent as main: everything above is your own history, and you continue from here on your own line. main keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to main, such as its list of open lines, are main's, not yours.

Your purpose, as main wrote it:
{purpose}

How you work with main:
- Work on this purpose only. You cannot open lines unless this line is the default branch of the session, and this block does not make it one: a status note would. If more parallel work is needed and you cannot open a line, tell main.
- Read main with the branch tool (action read, branch "main") when what main did or decided since your cut changes your work; the parent block reports when main has moved. Message main only for what it must act on: a decision that changes its work, a blocker only the user can settle, a conflict with another line, or a change to your purpose. Use mode "steer" for what main must act on now, "append" for what main should see at its next step. Do not send main a progress report it can read, and do not page through main's whole stream: read the page that answers your question, then act.
- You share the workspace with main and its other lines. Change only the files main gave you; before changing any other file, tell main which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell main before you continue.
- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.
- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. If you already told main that in a message, your end content carries only what is new. Read main once more before you end if the parent block says main has moved, so your last word still holds. That is your last action; main receives it.

This line is now the session's default branch, switched from line main. While this block is your latest one: answer the user here; you can open lines with create; you send no automatic idle notice to main; you cannot call end on yourself. Line main stays open and keeps the lines it opened.
</dim-branch-line>"#;

/// The product configures no note member, so the note a move writes carries
/// the protocol sentences and no `How you work here:` block, and the line's
/// next request carries that note byte for byte (timeline branches §12.1,
/// §12.4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_configuration_writes_no_guidance_block() {
    let purpose = "the purpose of B";
    let config = BranchToolConfig::default();
    assert!(
        config.default_note.is_none()
            && config.user_default_note.is_none()
            && config.former_default_note.is_none(),
        "the product configures no note member"
    );
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, config))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, purpose).await;
    switch(&session, &line).await;
    let notes = owed_notes(&session, &line, 1).await;
    let note = notes.last().expect("the line holds its note");
    let expected = PROTOCOL_ONLY_TOOL_NOTE
        .replace("{line}", &line)
        .replace("{purpose}", purpose);
    assert_eq!(
        note, &expected,
        "the default writes the note without a guidance block"
    );
    assert!(
        !note.contains("How you work here:"),
        "no guidance block without a configured one: {note}"
    );
    soon(session.submit("next request".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let events = local_events(&session, &line).await;
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .expect("the line's next turn makes a request");
    assert!(
        request
            .messages
            .iter()
            .any(|message| said(message).contains(&expected)),
        "the rendered request carries the note the default writes"
    );
    soon(agent.close()).await.unwrap();
}

/// The note a move writes on a line the user opened is the factory member
/// `BranchToolConfig::user_default_note`: the product sets no member, so the
/// note carries no `How you work here:` block, and a configured value reaches
/// the line's next model request with its `{parent}` and `{other}` placeholders
/// replaced (design §12.1, §12.4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_lines_default_note_reaches_the_model_request() {
    let block = "- Configured user guidance for {parent}, moved from {other}.";
    for (configured, heading) in [(None, false), (Some(block), true)] {
        let probe = Arc::new(Probe::default());
        let agent = MinimalHost::start(&builder_with_branch_config(
            &probe,
            BranchToolConfig {
                user_default_note: configured.map(Into::into),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let cut = idle_cut(&session, "main").await;
        let user = soon(session.branch(
            "main".into(),
            cut,
            BranchOptions {
                prompt: "review the docs".into(),
                source: BranchSource::User {
                    operation_id: "client".into(),
                },
            },
        ))
        .await
        .unwrap();
        quiet(&session, &user).await;
        switch(&session, &user).await;
        owed_notes(&session, &user, 1).await;
        soon(session.submit("next request".into(), InputMode::Queue, &user))
            .await
            .unwrap();
        soon(session.idle(&user)).await.unwrap();
        let events = local_events(&session, &user).await;
        let request = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            })
            .expect("the user line makes a request after the move");
        let note = request
            .messages
            .iter()
            .map(|message| said(message))
            .find(|text| {
                text.starts_with("<dim-branch-line ") && text.contains(" default=\"true\"")
            })
            .unwrap_or_else(|| panic!("the request carries the note: {request:?}"));
        assert!(
            note.starts_with(&format!(
                "<dim-branch-line branch=\"{user}\" parent=\"main\" source=\"user\" default=\"true\">"
            )),
            "{note}"
        );
        assert!(
            note.contains(
                "The user has now made this line the session's default branch, switched from line main."
            ),
            "{note}"
        );
        if heading {
            assert!(
                note.contains(
                    "\n\nHow you work here:\n- Configured user guidance for main, moved from main."
                ),
                "{note}"
            );
        } else {
            assert!(
                !note.contains("How you work here:"),
                "no guidance block without a configured one: {note}"
            );
        }
        soon(agent.close()).await.unwrap();
    }
}

/// The note a move writes on the former default branch is the factory member
/// `BranchToolConfig::former_default_note`, for a tool line too: the product
/// sets no member, so the note carries no `How you work here:` block, and a
/// configured value reaches the line's next model request with its `{parent}`
/// and `{other}` placeholders replaced by the line that holds the default
/// branch (design §12.1, §12.4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_former_default_tool_lines_note_reaches_the_model_request() {
    let block = "- Configured former guidance for {parent}, moved to {other}.";
    for (configured, heading) in [(None, false), (Some(block), true)] {
        let probe = Arc::new(Probe::default());
        let agent = MinimalHost::start(&builder_with_branch_config(
            &probe,
            BranchToolConfig {
                former_default_note: configured.map(Into::into),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let line = open_line(&session, "the parser race").await;
        switch(&session, &line).await;
        owed_notes(&session, &line, 1).await;
        let cut = idle_cut(&session, "main").await;
        let user = soon(session.branch(
            "main".into(),
            cut,
            BranchOptions {
                prompt: "the user line".into(),
                source: BranchSource::User {
                    operation_id: "client".into(),
                },
            },
        ))
        .await
        .unwrap();
        quiet(&session, &user).await;
        switch(&session, &user).await;
        owed_notes(&session, &line, 2).await;
        soon(session.submit("report what you found".into(), InputMode::Queue, &line))
            .await
            .unwrap();
        soon(session.idle(&line)).await.unwrap();
        let events = local_events(&session, &line).await;
        let request = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            })
            .expect("the former default line answers the input");
        let note = request
            .messages
            .iter()
            .map(|message| said(message))
            .find(|text| {
                text.starts_with("<dim-branch-line ") && text.contains(" default=\"false\"")
            })
            .unwrap_or_else(|| panic!("the request carries the note: {request:?}"));
        assert!(
            note.starts_with(&format!(
                "<dim-branch-line branch=\"{line}\" parent=\"main\" source=\"tool\" default=\"false\">"
            )),
            "{note}"
        );
        assert!(
            note.contains(&format!(
                "The session's default branch moved from this line to line {user}."
            )),
            "{note}"
        );
        if heading {
            assert!(
                note.contains(&format!(
                    "\n\nHow you work here:\n- Configured former guidance for main, moved to {user}."
                )),
                "{note}"
            );
        } else {
            assert!(
                !note.contains("How you work here:"),
                "no guidance block without a configured one: {note}"
            );
            assert!(
                note.ends_with("Only a line opened with create can end itself, and not while it or a line below it is the default branch.\n</dim-branch-line>"),
                "the shipped note ends with the end guard, not with an instruction: {note}"
            );
        }
        soon(agent.close()).await.unwrap();
    }
}

/// The note a move writes goes in as `append` input, and an append input never
/// starts a turn: the former default line, idle when the move happens, holds
/// the note as its head without making a request for it, and the next input is
/// what wakes it. The user-line scenario sends that input for this reason
/// (design §5, model-prompts §9.8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_appended_note_does_not_wake_the_idle_former_default() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(
        &probe,
        BranchToolConfig::default(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "the parser race").await;
    switch(&session, &line).await;
    owed_notes(&session, &line, 1).await;
    let cut = idle_cut(&session, "main").await;
    let user = soon(session.branch(
        "main".into(),
        cut,
        BranchOptions {
            prompt: "the user line".into(),
            source: BranchSource::User {
                operation_id: "client".into(),
            },
        },
    ))
    .await
    .unwrap();
    quiet(&session, &user).await;
    switch(&session, &user).await;
    owed_notes(&session, &line, 2).await;
    quiet(&session, &line).await;
    let events = local_events(&session, &line).await;
    let note_at = events
        .iter()
        .rev()
        .find(|event| {
            blocks_with(std::slice::from_ref(event), "<dim-branch-line ")
                .iter()
                .any(|text| text.contains(" default=\"false\""))
        })
        .map(|event| event.sequence)
        .expect("the line holds the former-default note");
    assert!(
        !events.iter().any(|event| {
            event.sequence > note_at
                && matches!(
                    event.kind,
                    EventKind::TurnStart(_) | EventKind::ModelCall(_)
                )
        }),
        "the appended note starts no turn and makes no request"
    );
    soon(session.submit("report what you found".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let events = local_events(&session, &line).await;
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) if event.sequence > note_at => Some(&data.request),
            _ => None,
        })
        .expect("the input wakes the line into a request");
    assert!(
        request
            .messages
            .iter()
            .any(|message| said(message)
                .contains("The session's default branch moved from this line")),
        "the woken request carries the note: {request:?}"
    );
    soon(agent.close()).await.unwrap();
}

/// #549: a description set at the declaring owner's factory configuration
/// reaches the model request byte for byte, carried through ToolRegistry and
/// ToolBinding by the shipped assembly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_description_reaches_the_request_declaration() {
    let description =
        "Work with the other lines of this session. Configured for the #549 flow test.";
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(
        &probe,
        BranchToolConfig {
            description: Some(description.into()),
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let tools = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.tools.clone(),
            _ => None,
        })
        .expect("the request carries the declarations");
    let branch = tools
        .iter()
        .find(|tool| tool.name == "branch")
        .expect("the branch tool is declared");
    assert_eq!(branch.description, description);
    soon(agent.close()).await.unwrap();
}

/// #552: `<dim-branch-line>` is written by the Session part TimelineTree, so
/// its guide entry is registered there, at the Session, and a line carries it
/// even where no branch part is loaded. `<dim-branch-lines>` stays with
/// BranchLine, which writes the roster.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="P" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; a block carrying `default` is the note a move writes. Both are state about the line, not requests."#;
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="O">: the lines you opened, under the key `branch:roster` and apart from your own identity under `branch:line`: the runtime lists the tool lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;
/// #987: the parent block can arrive only on a line with a parent, so main's
/// guide carries no entry for it.
const PARENT_ENTRY: &str = r#"<dim-branch-parent branch="P" head="H" after="A">: on a line, the runtime reports that the line it was cut from has moved past where this line last read it, and where the next read starts. A later block of this name replaces the earlier one; it is state about that line, not a request."#;
const MESSAGE_ENTRY: &str = r#"<dim-branch-message from="C" to="P">: a message from another line of this session, in that line's own words and not the operator's. dim-branch-idle and dim-branch-ended are the runtime's reports about a line you opened: information, not requests."#;

fn system_text(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Parts(parts) => parts
            .iter()
            .map(|part| {
                let SystemPromptPart::Text { text, .. } = part;
                text.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The system of a branch's last model call.
async fn last_system(session: &Session, branch: &str) -> String {
    chain_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.system.as_deref().map(system_text),
            _ => None,
        })
        .expect("a model call carries the system")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_line_carries_the_entry_of_the_marker_its_part_writes() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();

    // main writes the roster and sees it: its marker and its entry. The
    // entries registered for the whole Session are on main too.
    let main = last_system(&session, "main").await;
    assert_eq!(main.matches(IDENTITY_ENTRY).count(), 1, "{main:?}");
    assert_eq!(main.matches(ROSTER_ENTRY).count(), 1, "{main:?}");
    assert_eq!(main.matches(MESSAGE_ENTRY).count(), 1, "{main:?}");
    assert_eq!(main.matches(PARENT_ENTRY).count(), 1, "{main:?}");
    let roster = roster_block(&chain_events(&session, "main", ..).await.unwrap())
        .expect("the roster is keyed `branch:roster`");
    assert!(roster.0.starts_with("<dim-branch-lines"));
    let identity = line_block(&local_events(&session, "main").await);
    assert!(
        identity.is_none(),
        "the root writes no identity: {identity:?}"
    );

    // The line carries its identity marker, and the Session's writer
    // explains it there.
    let opening = line_block(&chain_events(&session, &child, ..).await.unwrap())
        .expect("the line's opening input carries its identity");
    assert!(opening.0.starts_with(&format!(
        "<dim-branch-line branch=\"{child}\" parent=\"main\" source=\"tool\">"
    )));
    let line = last_system(&session, &child).await;
    assert_eq!(line.matches(IDENTITY_ENTRY).count(), 1, "{line:?}");
    assert_eq!(line.matches(PARENT_ENTRY).count(), 1, "{line:?}");
    soon(agent.close()).await.unwrap();
}

/// The system of a branch's first model call on the branch itself.
async fn first_system(session: &Session, branch: &str) -> String {
    local_events(session, branch)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.system.as_deref().map(system_text),
            _ => None,
        })
        .expect("the first model call carries the system")
}

/// The system of a branch's last model call at or before `sequence`.
async fn system_at(session: &Session, branch: &str, sequence: u64) -> String {
    local_events(session, branch)
        .await
        .iter()
        .filter(|event| event.sequence <= sequence)
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.system.as_deref().map(system_text),
            _ => None,
        })
        .expect("a model call at the cut carries the system")
}

/// A child's first request reuses its parent's cached prefix: its System is
/// the parent's System in the request that opened it, byte for byte
/// ([§7.3](../../../docs/design/timeline-branches.md#73-cache-prefix)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_childs_first_system_is_its_parents_at_the_cut() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    let opened = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .create_anchor
        .unwrap();
    let parent = system_at(&session, "main", opened).await;
    let first = first_system(&session, &child).await;
    assert_eq!(
        first, parent,
        "the child's first System is its parent's at the cut"
    );
    soon(agent.close()).await.unwrap();
}

/// #637: the workspace rule names the stale copy in the words the result
/// carries. The tool's side is a write through the line's own read binding,
/// the guard the design relies on (§8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_names_the_stale_file_failure_the_way_the_tool_reports_it() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    let history = chain_events(&session, &child, ..).await.unwrap();
    let (identity, _) = line_block(&history).unwrap();
    assert!(
        identity.contains(
            "If edit or write fails because the file was modified after it was last read"
        ),
        "{identity:?}"
    );

    let root = std::env::temp_dir().join(format!("dim-branch-workspace-{}", uuid::Uuid::new_v4()));
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let tools = FileTools::new(work.clone(), root.clone(), root.join("outputs"));
    let timeline = probe
        .child_timeline
        .lock()
        .unwrap()
        .clone()
        .expect("the line's Timeline");
    let reads = FileReads::new(timeline);
    std::fs::write(work.join("shared.txt"), "first\n").unwrap();
    let read = file_call(
        &tools,
        &reads,
        "read",
        serde_json::json!({"path": "shared.txt"}),
    )
    .await;
    assert_eq!(read.is_error, None, "{:?}", read.content);
    std::fs::write(work.join("shared.txt"), "changed elsewhere\n").unwrap();
    let stale = file_call(
        &tools,
        &reads,
        "write",
        serde_json::json!({"path": "shared.txt", "content": "mine\n"}),
    )
    .await;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(
        stale.content,
        ToolContent::Text(
            "File was modified after it was last read. Use the read tool again before writing."
                .into()
        )
    );
    soon(agent.close()).await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

async fn file_call(
    tools: &FileTools,
    reads: &FileReads,
    name: &str,
    input: serde_json::Value,
) -> ToolResult {
    let call = ToolCall {
        id: "call".into(),
        name: name.into(),
        input: input.as_object().unwrap().clone(),
    };
    tools
        .execute(&call, reads, CancellationToken::new())
        .await
        .unwrap()
}

/// #552: the branch tool's actions, modes and schema are protocol, so they
/// are asserted here; the description's policy sentences are judged by the
/// §12.3 evaluation instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_branch_tool_declares_its_actions_and_modes() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let tools = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.tools.clone(),
            _ => None,
        })
        .expect("the request carries the declarations");
    let branch = tools
        .iter()
        .find(|tool| tool.name == "branch")
        .expect("the branch tool is declared");
    assert_eq!(branch.risk.as_deref(), Some("session"));
    let schema = serde_json::to_value(&branch.input_schema).unwrap();
    assert_eq!(schema["required"], serde_json::json!(["action"]));
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    assert_eq!(
        schema["properties"]["action"]["enum"],
        serde_json::json!(["create", "message", "read", "end"])
    );
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        serde_json::json!(["append", "steer"])
    );
    assert_eq!(schema["properties"]["at"]["minimum"], serde_json::json!(0));
    assert_eq!(
        schema["properties"]["after"]["minimum"],
        serde_json::json!(0)
    );
    assert_eq!(
        schema["properties"]["limit"]["minimum"],
        serde_json::json!(1)
    );
    assert_eq!(
        schema["properties"]["limit"]["maximum"],
        serde_json::json!(200)
    );
    for sentence in [
        "- create (the root and the current default branch only): open a line to work on prompt in parallel.",
        "- message: send content to a line you opened, or to the line that opened you.",
        "Mode \"append\" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line.",
        "- read: show a page of the activity of the line you were cut from, or of a line you opened, also after it ended.",
        "With no after, a read continues after your last read of that line: from your cut for the line you were cut from, and from its first event for a line you opened.",
        "Pass after to read from another point.",
        "- end: on a line opened with create, end your own line.",
        "Returns the line's id.",
        "It is your last action.",
        "A line messages and reads the line it was cut from and the lines it opened. The end action ends the calling line and every line below it.",
    ] {
        assert!(
            branch.description.contains(sentence),
            "{sentence:?} is missing from: {}",
            branch.description
        );
    }
    soon(agent.close()).await.unwrap();
}

/// #552: a malformed call is a refusal of the tool's own argument rules.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_branch_tool_reports_invalid_arguments() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    for (arguments, text) in [
        (
            r#"{"action":"message","content":"x"}"#,
            "Invalid branch arguments: message requires to",
        ),
        (
            r#"{"action":"frobnicate"}"#,
            "Invalid branch arguments: unknown action frobnicate",
        ),
    ] {
        let result = tool_call(&session, "main", arguments).await;
        assert_eq!(result.is_error, Some(true), "{:?}", result.content);
        assert!(
            matches!(&result.content, ToolContent::Text(shown) if shown == text),
            "{:?}",
            result.content
        );
    }
    soon(agent.close()).await.unwrap();
}

/// #2000: `title` is no argument of the branch tool, and the refusal says what
/// names a line; a call that leaves `title` out opens a line named by the
/// prompt's first line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_is_refused_with_what_names_a_line() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let refused = tool_call(
        &session,
        "main",
        r#"{"action":"create","title":"watch the tests","prompt":"watch the tests\nreport once"}"#,
    )
    .await;
    assert_eq!(refused.is_error, Some(true), "{:?}", refused.content);
    assert!(
        matches!(
            &refused.content,
            ToolContent::Text(shown)
                if shown
                    == "Invalid branch arguments: no `title` parameter; a line's name is the \
                        first line of `prompt`."
        ),
        "{:?}",
        refused.content
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    let tools = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.tools.clone(),
            _ => None,
        })
        .expect("the request carries the declarations");
    let branch = tools
        .iter()
        .find(|tool| tool.name == "branch")
        .expect("the branch tool is declared");
    let schema = serde_json::to_value(&branch.input_schema).unwrap();
    assert_eq!(
        schema["properties"]["prompt"]["description"],
        serde_json::json!(
            "create: the purpose of the new line; its first line is the line's name."
        )
    );
    let mut properties: Vec<&str> = schema["properties"]
        .as_object()
        .expect("the schema declares properties")
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        [
            "action",
            "after",
            "at",
            "branch",
            "content",
            "idleNotice",
            "limit",
            "mode",
            "prompt",
            "to"
        ]
    );
    let child = open_line(&session, "watch the tests\nreport once").await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let created: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreate(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        created.len(),
        1,
        "the refused call opened no line: {created:?}"
    );
    assert_eq!(created[0].branch, child);
    assert_eq!(created[0].prompt, "watch the tests\nreport once");
    let events = until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-lines")
            .last()
            .is_some_and(|text| text.contains(&child))
    })
    .await;
    let (roster, _) = roster_block(&events).expect("the roster is keyed `branch:roster`");
    assert!(
        roster.contains(&format!("\n- {child}: watch the tests\n")),
        "the line is named by the prompt's first line: {roster}"
    );
    assert!(!roster.contains("report once"));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_reaches_its_receiver_in_the_mode_it_asked_for() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let result = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"message","to":"{child}","content":"tests are green"}}"#),
    )
    .await;
    assert_eq!(
        result.content,
        ToolContent::Text(format!("Sent to {child}."))
    );
    let events = until_events(&session, &child, |events| {
        !blocks_with(events, "<dim-branch-message").is_empty()
    })
    .await;
    let (sequence, mode) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if data.blocks.iter().any(|block| {
                    matches!(block, InputBlock::Raw { text, .. }
                        if text.starts_with("<dim-branch-message"))
                }) =>
            {
                Some((event.sequence, data.effective_mode()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(mode, InputMode::Append);
    assert!(
        blocks_with(&events, "<dim-branch-message")
            .last()
            .unwrap()
            .starts_with(&format!(
                "<dim-branch-message from=\"main\" to=\"{child}\">\ntests are green\n</dim-branch-message>"
            ))
    );
    assert!(
        !events.iter().any(|event| {
            matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > sequence
        }),
        "an append starts no turn"
    );
    tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"message","to":"{child}","content":"act now","mode":"steer"}}"#),
    )
    .await;
    let events = until_events(&session, &child, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)))
    })
    .await;
    assert!(
        blocks_with(&events, "<dim-branch-message")
            .last()
            .unwrap()
            .contains("act now")
    );
    soon(session.idle(&child)).await.unwrap();
    let before = session.sequence("main").unwrap();
    tool_call(
        &session,
        &child,
        r#"{"action":"message","to":"main","content":"the leak is in cache.rs"}"#,
    )
    .await;
    let events = until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-message")
            .iter()
            .any(|text| text.contains("the leak is in cache.rs"))
    })
    .await;
    let landed = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if blocks_with(std::slice::from_ref(event), "<dim-branch-message")
                    .iter()
                    .any(|text| text.contains("the leak is in cache.rs")) =>
            {
                Some((event.sequence, data.effective_mode()))
            }
            _ => None,
        })
        .expect("the message landed on main");
    assert!(landed.0 > before);
    assert_eq!(landed.1, InputMode::Append);
    assert!(
        blocks_with(&events, "<dim-branch-message")
            .iter()
            .any(|text| text.starts_with(&format!(
                "<dim-branch-message from=\"{child}\" to=\"main\">"
            )))
    );
    let idle_at = events
        .iter()
        .find(|event| !blocks_with(std::slice::from_ref(event), "<dim-branch-idle").is_empty())
        .map(|event| event.sequence);
    if let Some(start) = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > landed.0)
        .map(|event| event.sequence)
    {
        assert!(
            idle_at.is_some_and(|notice| notice < start),
            "an append from a line starts no turn on main"
        );
    }
    until_events(&session, "main", |events| {
        events.iter().any(|event| {
            event.sequence > landed.0
                && !blocks_with(std::slice::from_ref(event), "<dim-branch-idle").is_empty()
        })
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    tool_call(
        &session,
        &child,
        r#"{"action":"message","to":"main","content":"found the leak","mode":"steer"}"#,
    )
    .await;
    let events = until_events(&session, "main", |events| {
        let Some(steer) = events.iter().find(|event| {
            blocks_with(std::slice::from_ref(event), "<dim-branch-message")
                .iter()
                .any(|text| text.contains("found the leak"))
        }) else {
            return false;
        };
        events.iter().any(|event| {
            matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > steer.sequence
        })
    })
    .await;
    let steer = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if blocks_with(std::slice::from_ref(event), "<dim-branch-message")
                    .iter()
                    .any(|text| text.contains("found the leak")) =>
            {
                Some((event.sequence, data.effective_mode()))
            }
            _ => None,
        })
        .expect("the steer landed on main");
    assert_eq!(steer.1, InputMode::Steer);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > steer.0),
        "a steer starts a turn on an idle main"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_parent_read_starts_after_the_cut() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("BEFORE-THE-CUT".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let child = open_line(&session, "check the parser").await;
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap();
    let cut = info.branch_point.unwrap();
    let result = tool_call(&session, &child, r#"{"action":"read","branch":"main"}"#).await;
    let page = page_of(&result);
    let sequences = page_sequences(&page);
    assert!(!sequences.is_empty(), "{page}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cut),
        "the first parent read starts after the cut {cut}: {page}"
    );
    let text = page.to_string();
    assert!(
        !text.contains("BEFORE-THE-CUT"),
        "the inherited events are not the parent's new activity: {page}"
    );
    assert!(
        page["nextCursor"].as_u64().unwrap() >= *sequences.last().unwrap(),
        "the cursor covers the page: {page}"
    );
    soon(agent.close()).await.unwrap();
}

/// A tool whose result declares the effects its input names, the way a
/// workspace tool declares its own result (session boundary A.6).
struct EffectTools;
impl ToolDriver for EffectTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..dim_agent_capabilities::tool_registry::definition(
                "touch",
                "Declare the file or the command the input names.",
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": {"type": "string"},
                        "command": {"type": "string"},
                    }
                }),
            )
        }]
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let mut effects = Vec::new();
            if let Some(path) = call.input.get("path").and_then(serde_json::Value::as_str) {
                effects.push(ToolEffect::FileWritten {
                    path: path.to_owned(),
                });
            }
            if let Some(command) = call
                .input
                .get("command")
                .and_then(serde_json::Value::as_str)
            {
                effects.push(ToolEffect::CommandRun {
                    command: command.to_owned(),
                    exit_code: Some(0),
                    signal: None,
                });
            }
            let mut result = ToolResult::text("large result content".repeat(32768));
            result.effects = (!effects.is_empty()).then_some(effects);
            Ok(result)
        })
    }
}

/// A page carries the effects its parent's tool results declared: the path
/// verbatim, a command cut at 200 characters with the cut marked, and no
/// `effects` member when the result declared none; the `tool_call` items are
/// unchanged (#1033).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parent_read_carries_the_effects_its_tool_results_declared() {
    let probe = Arc::new(Probe::default());
    let store = Arc::new(TestStore::refusing_ranges(Arc::new(
        MemoryTimelineStore::default(),
    )));
    let mut builder = builder_with_tools(&probe, Arc::new(EffectTools));
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec![
            "echo".into(),
            "wait".into(),
            "branch".into(),
            "touch".into(),
        ]),
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    soon(session.submit("BEFORE-THE-CUT".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let child = open_line(&session, "check the parser").await;
    let path = "/work/src/parse.py";
    let command = format!("cargo test --locked {}", "x".repeat(200));
    for call in [
        format!(r#"tool:touch {{"path":"{path}"}}"#),
        format!(r#"tool:touch {{"command":"{command}"}}"#),
        r#"tool:touch {"note":"neither"}"#.to_string(),
    ] {
        soon(session.submit(call, InputMode::Queue, "main"))
            .await
            .unwrap();
        soon(session.idle("main")).await.unwrap();
    }
    let page = page_of(&tool_call(&session, &child, r#"{"action":"read","branch":"main"}"#).await);
    assert_eq!(
        store.range_reads(),
        0,
        "branch reads must not decode whole stored events"
    );
    let items = page["items"].as_array().expect("page items");
    let results: Vec<&serde_json::Value> = items
        .iter()
        .filter(|item| item["type"] == "tool_result" && item["name"] == "touch")
        .collect();
    assert_eq!(results.len(), 3, "{page}");
    assert_eq!(
        results[0]["effects"],
        serde_json::json!([{"kind": "fileWritten", "path": path}]),
        "{page}"
    );
    let command_effect = &results[1]["effects"][0];
    assert_eq!(command_effect["kind"], "commandRun");
    let shortened = command_effect["command"].as_str().expect("command");
    assert_eq!(shortened.chars().count(), 201, "{page}");
    assert_eq!(
        shortened.strip_suffix('…').expect("the cut is marked"),
        &command[..200],
        "{page}"
    );
    assert_eq!(command_effect["exitCode"], 0);
    assert!(results[2].get("effects").is_none(), "{page}");
    let calls: Vec<&serde_json::Value> = items
        .iter()
        .filter(|item| item["type"] == "tool_call")
        .collect();
    assert!(calls.len() >= 3, "{page}");
    for call in calls {
        let keys: Vec<&String> = call.as_object().expect("call item").keys().collect();
        assert_eq!(keys, ["sequence", "type", "callId", "name"], "{call}");
    }
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_after_below_the_cut_is_raised_to_it() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("BEFORE-THE-CUT".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let child = open_line(&session, "check the parser").await;
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .branch_point
        .unwrap();
    let page = page_of(
        &tool_call(
            &session,
            &child,
            r#"{"action":"read","branch":"main","after":0}"#,
        )
        .await,
    );
    let sequences = page_sequences(&page);
    assert!(!sequences.is_empty(), "{page}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cut),
        "after 0 is raised to the cut {cut}: {page}"
    );
    assert!(!page.to_string().contains("BEFORE-THE-CUT"), "{page}");
    soon(agent.close()).await.unwrap();
}

/// An `after` past the target's head is a mistake, so the read refuses it and
/// stores nothing, keeping the stored cursor valid by construction
/// ([§20.11](../../../docs/design/timeline-branches.md#20-acceptance)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_after_past_the_targets_head_is_refused_and_the_cursor_stays() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    // A host line writes no idle notice, so main has no turn in flight and
    // its head stays where the test reads it.
    let line = open_host_line(&session, "main", "check the parser").await;
    let first = page_of(&tool_call(&session, &line, r#"{"action":"read","branch":"main"}"#).await);
    let cursor = first["nextCursor"].as_u64().unwrap();
    let head = session.sequence("main").unwrap();
    let refused = tool_call(
        &session,
        &line,
        &format!(
            r#"{{"action":"read","branch":"main","after":{}}}"#,
            head + 1
        ),
    )
    .await;
    assert_eq!(refused.is_error, Some(true), "{:?}", refused.content);
    assert!(
        matches!(&refused.content, ToolContent::Text(text) if text.contains(&head.to_string())),
        "the refusal names the head {head}: {:?}",
        refused.content
    );
    move_main(&session).await;
    let (_, text, _) = settle_line(&session, &line).await;
    let (block_head, after) = parent_block(&text);
    assert_eq!(after, cursor, "the refused read stored no cursor");
    assert!(block_head > cursor);
    let second = page_of(&tool_call(&session, &line, r#"{"action":"read","branch":"main"}"#).await);
    let sequences = page_sequences(&second);
    assert!(!sequences.is_empty(), "{second}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cursor),
        "the read continues after the cursor {cursor}: {second}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_without_after_continues_from_the_last_page_in_both_directions() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "check the parser").await;
    // line to main: a partial page, then main moves, then a read with no after.
    let first = page_of(
        &tool_call(
            &session,
            &child,
            r#"{"action":"read","branch":"main","limit":2}"#,
        )
        .await,
    );
    let cursor = first["nextCursor"].as_u64().unwrap();
    soon(session.submit("AFTER-THE-FIRST-PAGE".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let second =
        page_of(&tool_call(&session, &child, r#"{"action":"read","branch":"main"}"#).await);
    let sequences = page_sequences(&second);
    assert!(!sequences.is_empty(), "{second}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cursor),
        "the second read continues after {cursor}: {second}"
    );
    assert!(
        second.to_string().contains("AFTER-THE-FIRST-PAGE"),
        "the second read covers the parent's new activity: {second}"
    );
    // main to line: the same rule.
    let first = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}","limit":2}}"#),
        )
        .await,
    );
    let cursor = first["nextCursor"].as_u64().unwrap();
    soon(session.submit("AFTER-THE-CHILD-PAGE".into(), InputMode::Queue, &child))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let second = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}"}}"#),
        )
        .await,
    );
    let sequences = page_sequences(&second);
    assert!(!sequences.is_empty(), "{second}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cursor),
        "main's second read continues after {cursor}: {second}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn main_re_reads_a_line_from_zero_and_the_cursor_follows_that_read() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "check the parser").await;
    let zero = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}","after":0,"limit":2}}"#),
        )
        .await,
    );
    let sequences = page_sequences(&zero);
    let prompt_on_first_page = zero.to_string().contains("check the parser");
    if !prompt_on_first_page {
        assert!(
            sequences.is_empty(),
            "the opening page contains only hidden facts: {zero}"
        );
        assert_eq!(zero["done"], false, "hidden facts do not finish the read");
    }
    let cursor = zero["nextCursor"].as_u64().unwrap();
    assert!(cursor > 0 && cursor >= sequences.last().copied().unwrap_or(0));
    let next = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}"}}"#),
        )
        .await,
    );
    let sequences = page_sequences(&next);
    if !prompt_on_first_page {
        assert!(
            next.to_string().contains("check the parser"),
            "the next page reaches the opening input: {next}"
        );
    }
    assert!(
        sequences.iter().all(|sequence| *sequence > cursor),
        "the next read continues after {cursor}: {next}"
    );
    let again = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}","after":0,"limit":2}}"#),
        )
        .await,
    );
    assert_eq!(
        again["items"], zero["items"],
        "after 0 re-reads the opening page"
    );
    assert_eq!(again["nextCursor"], zero["nextCursor"]);
    assert_eq!(page_sequences(&again), page_sequences(&zero));
    let cursor = again["nextCursor"].as_u64().unwrap();
    let next = page_of(
        &tool_call(
            &session,
            "main",
            &format!(r#"{{"action":"read","branch":"{child}"}}"#),
        )
        .await,
    );
    assert!(
        page_sequences(&next)
            .iter()
            .all(|sequence| *sequence > cursor),
        "the cursor follows the explicit read: {next}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_cursor_survives_a_restore() {
    let store = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id().to_string();
    let child = open_line(&session, "check the parser").await;
    let page = page_of(
        &tool_call(
            &session,
            &child,
            r#"{"action":"read","branch":"main","limit":2}"#,
        )
        .await,
    );
    let cursor = page["nextCursor"].as_u64().unwrap();
    soon(agent.close()).await.unwrap();
    let agent = MinimalHost::start(&builder_with_store(&probe, store))
        .await
        .unwrap();
    let session = soon(agent.restore_session(id)).await.unwrap();
    until_branch(&session, &child).await;
    soon(session.idle(&child)).await.unwrap();
    let page = page_of(&tool_call(&session, &child, r#"{"action":"read","branch":"main"}"#).await);
    let sequences = page_sequences(&page);
    assert!(!sequences.is_empty(), "{page}");
    assert!(
        sequences.iter().all(|sequence| *sequence > cursor),
        "the restored fold continues after {cursor}: {page}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_line_is_told_its_parent_moved_without_being_woken() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    quiet(&session, &child).await;
    move_main(&session).await;
    let (block, text, _) = settle_line(&session, &child).await;
    let (head, after) = parent_block(&text);
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .branch_point
        .unwrap();
    let main_events = local_events(&session, "main").await;
    assert!(
        main_events.iter().any(|event| {
            matches!(event.kind, EventKind::TurnEnd(_))
                && event.sequence > cut
                && event.sequence <= head
        }),
        "the block follows a turn end of the parent"
    );
    assert!(head <= session.sequence("main").unwrap());
    assert_eq!(after, cut);
    let events = local_events(&session, &child).await;
    let mode = events
        .iter()
        .find(|event| event.sequence == block)
        .and_then(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.effective_mode()),
            _ => None,
        })
        .unwrap();
    assert_eq!(mode, InputMode::Append, "the block waits for the line");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > block),
        "the block does not wake an idle line"
    );
    soon(session.submit("ACT-ON-IT".into(), InputMode::Queue, &child))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let events = local_events(&session, &child).await;
    let request = events
        .iter()
        .find(|event| event.sequence > block && matches!(event.kind, EventKind::ModelCall(_)))
        .expect("the next turn made a request");
    let EventKind::ModelCall(data) = &request.kind else {
        unreachable!()
    };
    let messages = &data.request.messages;
    let block_at = messages
        .iter()
        .position(|message| said(message) == text)
        .expect("the block is in the first request of the next turn");
    let texts: Vec<String> = messages
        .iter()
        .map(|message| format!("{:?}: {:?}", message.role, said(message)))
        .collect();
    let input_at = messages
        .iter()
        .position(|message| said(message) == "ACT-ON-IT")
        .unwrap_or_else(|| panic!("the input is in the request: {texts:#?}"));
    assert!(block_at < input_at, "the block comes before the input");
    assert_eq!(
        messages
            .iter()
            .filter(|message| said(message) == text)
            .count(),
        1,
        "the block is in the request once"
    );
    assert_eq!(
        parent_block_events(&events)
            .iter()
            .filter(|(sequence, _)| *sequence > block)
            .count(),
        0,
        "no second block was written for the same head"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_line_gets_the_block_with_its_next_request_once() {
    let probe = Arc::new(Probe::default());
    let gates = Arc::new(GateModel::default());
    let agent = MinimalHost::start(&builder_with_gates(&probe, &gates))
        .await
        .unwrap();
    let session = patient(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    // The creating turn only answered the call that opened the line, so no
    // block is held yet; the gated turn below is the movement.
    let events = local_events(&session, &child).await;
    assert!(parent_block_events(&events).is_empty(), "{events:#?}");
    // main is held in a turn, so only it moves the line's parent when released.
    patient(session.submit("GATE-MAIN".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    patient(gates.main_entered.notified()).await;
    let line_head = session.sequence(&child).unwrap();
    // the line is running, held at its own gate.
    patient(session.submit("GATE-LINE".into(), InputMode::Queue, &child))
        .await
        .unwrap();
    patient(gates.line_entered.notified()).await;
    gates.main_release.notify_one();
    patient(session.idle("main")).await.unwrap();
    let head = session.sequence("main").unwrap();
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .branch_point
        .unwrap();
    let events = until_events(&session, &child, |events| {
        parent_block_events(events)
            .iter()
            .any(|(sequence, _)| *sequence > line_head)
    })
    .await;
    let written: Vec<(u64, String)> = parent_block_events(&events)
        .into_iter()
        .filter(|(sequence, _)| *sequence > line_head)
        .collect();
    assert_eq!(written.len(), 1, "{written:?}");
    let (written_head, after) = parent_block(&written[0].1);
    assert!(written_head <= head, "the head at the write: {written:?}");
    assert_eq!(after, cut);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnEnd(_)) && event.sequence > line_head),
        "the line is still running while the block lands"
    );
    gates.line_release.notify_one();
    patient(session.idle(&child)).await.unwrap();
    let events = local_events(&session, &child).await;
    let requests: Vec<&Arc<AgentEvent>> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
        .collect();
    let carries = |event: &&Arc<AgentEvent>| {
        let EventKind::ModelCall(data) = &event.kind else {
            return false;
        };
        data.request
            .messages
            .iter()
            .any(|message| said(message) == written[0].1)
    };
    let carried = requests
        .iter()
        .filter(|event| event.sequence > written[0].0)
        .find(|event| carries(event))
        .unwrap_or_else(|| panic!("a request after the block carries it: {written:?}"));
    let EventKind::ModelCall(data) = &carried.kind else {
        unreachable!()
    };
    assert_eq!(
        data.request
            .messages
            .iter()
            .filter(|message| said(message) == written[0].1)
            .count(),
        1,
        "the request carries the block once"
    );
    let at = requests
        .iter()
        .position(|event| event.sequence == carried.sequence)
        .unwrap();
    if at > 0 {
        let EventKind::ModelCall(previous) = &requests[at - 1].kind else {
            unreachable!()
        };
        assert!(
            !previous
                .request
                .messages
                .iter()
                .any(|message| said(message) == written[0].1),
            "the request before it is built before the block"
        );
    }
    patient(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_parent_turns_while_the_line_is_idle_write_one_block() {
    let probe = Arc::new(Probe::default());
    let gates = Arc::new(GateModel::default());
    let agent = MinimalHost::start(&builder_with_gates(&probe, &gates))
        .await
        .unwrap();
    let session = patient(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    quiet(&session, &child).await;
    move_main(&session).await;
    let (block, _, _) = settle_line(&session, &child).await;
    for _ in 0..2 {
        patient(session.submit("GATE-MAIN".into(), InputMode::Queue, "main"))
            .await
            .unwrap();
        patient(gates.main_entered.notified()).await;
        gates.main_release.notify_one();
        patient(session.idle("main")).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = local_events(&session, &child).await;
    assert_eq!(
        parent_block_events(&events)
            .iter()
            .filter(|(sequence, _)| *sequence > block)
            .count(),
        0,
        "a held block is not written over"
    );
    assert!(
        block_held(&events, block),
        "the first block is still waiting"
    );
    patient(agent.close()).await.unwrap();
}

/// A host line under `parent`, opened at its current head and settled.
async fn open_host_line(session: &Session, parent: &str, purpose: &str) -> String {
    let at = session.sequence(parent).unwrap();
    let line = soon(session.branch(parent.into(), at, options(purpose)))
        .await
        .unwrap();
    quiet(session, &line).await;
    line
}

/// The parent blocks on `branch` once one follows `after`, or a panic naming
/// what was there instead.
async fn second_parent_block(session: &Session, branch: &str, after: u64) -> Vec<(u64, String)> {
    let mut events = Vec::new();
    for _ in 0..300 {
        events = local_events(session, branch).await;
        if parent_block_events(&events)
            .iter()
            .any(|(sequence, _)| *sequence > after)
        {
            return parent_block_events(&events);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "no block was written after block {after}: {:#?}",
        parent_block_events(&events)
    );
}

/// A parent move that lands while a block is held is written once the line
/// consumes that block, with no later parent turn
/// ([§20.12](../../../docs/design/timeline-branches.md#20-acceptance)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_while_a_block_is_held_is_written_after_the_block_is_consumed() {
    let probe = Arc::new(Probe::default());
    let gates = Arc::new(GateModel::default());
    let agent = MinimalHost::start(&builder_with_gates(&probe, &gates))
        .await
        .unwrap();
    let session = patient(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    // A host line writes no idle notice, so main's turn stays where the test
    // holds it and cannot be the signal that writes the next block.
    let line = open_host_line(&session, "main", "watch the tests").await;
    move_main(&session).await;
    let (first, _, first_head) = settle_line(&session, &line).await;
    move_main(&session).await;
    let moved = session.sequence("main").unwrap();
    assert!(moved > first_head, "the parent moved past the held block");
    let events = local_events(&session, &line).await;
    assert!(block_held(&events, first), "the next move finds it held");
    assert_eq!(
        parent_block_events(&events)
            .iter()
            .filter(|(sequence, _)| *sequence > first)
            .count(),
        0,
        "a held block is not written over"
    );
    patient(session.submit("GATE-MAIN".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    patient(gates.main_entered.notified()).await;
    patient(session.submit("consume".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    patient(session.idle(&line)).await.unwrap();
    let blocks = second_parent_block(&session, &line, first).await;
    let (second_head, _) = parent_block(&blocks.last().unwrap().1);
    assert!(
        second_head >= moved,
        "the block names the head the parent reached while the first was held: {blocks:?}"
    );
    gates.main_release.notify_one();
    patient(session.idle("main")).await.unwrap();
    patient(agent.close()).await.unwrap();
}

/// A deleted parent block releases the move that arrived while it was held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_while_a_block_is_held_is_written_after_the_block_is_deleted() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_host_line(&session, "main", "watch the tests").await;
    move_main(&session).await;
    let (first, _, first_head) = settle_line(&session, &line).await;
    move_main(&session).await;
    let moved = session.sequence("main").unwrap();
    assert!(moved > first_head, "the parent moved past the held block");
    let events = local_events(&session, &line).await;
    let block = events
        .iter()
        .find(|event| event.sequence == first)
        .expect("the held block");
    let timeline = probe
        .child_timeline
        .lock()
        .unwrap()
        .clone()
        .expect("the line's Timeline");
    timeline
        .append(
            EventKind::InputDeleted(InputDeletedData {
                input_id: block.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let blocks = second_parent_block(&session, &line, first).await;
    let (second_head, _) = parent_block(&blocks.last().unwrap().1);
    assert!(
        second_head >= moved,
        "the block names the head the parent reached while the first was held: {blocks:?}"
    );
    patient(agent.close()).await.unwrap();
}

/// A parent block whose turn is interrupted still reports the move the held
/// block swallowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_while_a_block_is_held_is_written_after_the_block_is_interrupted() {
    let probe = Arc::new(Probe::default());
    let gates = Arc::new(GateModel::default());
    let agent = MinimalHost::start(&builder_with_gates(&probe, &gates))
        .await
        .unwrap();
    let session = patient(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_host_line(&session, "main", "watch the tests").await;
    move_main(&session).await;
    let (first, _, first_head) = settle_line(&session, &line).await;
    move_main(&session).await;
    let moved = session.sequence("main").unwrap();
    assert!(moved > first_head, "the parent moved past the held block");
    let events = local_events(&session, &line).await;
    let block = events
        .iter()
        .find(|event| event.sequence == first)
        .expect("the held block");
    patient(session.submit("GATE-LINE".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    patient(gates.line_entered.notified()).await;
    let stopped = session.interrupt(&line, "stop".into()).unwrap();
    let applied = local_events(&session, &line)
        .await
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == block.id)
        })
        .map(|event| event.sequence);
    assert!(
        applied.is_some_and(|at| at <= stopped),
        "the turn held the block when it was interrupted"
    );
    gates.line_release.notify_one();
    patient(session.idle(&line)).await.unwrap();
    let blocks = second_parent_block(&session, &line, first).await;
    let (second_head, _) = parent_block(&blocks.last().unwrap().1);
    assert!(
        second_head >= moved,
        "the block names the head the parent reached while the first was held: {blocks:?}"
    );
    patient(agent.close()).await.unwrap();
}

/// The blocks of a line's opening input: the message that carries its
/// identity, each as its key, its text and whether it retires the key.
fn opening_blocks(events: &[Arc<AgentEvent>]) -> Vec<(Option<String>, String, bool)> {
    events
        .iter()
        .find_map(|event| {
            match &event.kind {
            EventKind::InputMessage(data)
                if data.blocks.iter().any(|block| {
                    matches!(block, InputBlock::Raw { key: Some(key), .. } if key == "branch:line")
                }) =>
            {
                Some(
                    data.blocks
                        .iter()
                        .map(|block| {
                            let InputBlock::Raw { text, key, retire } = block;
                            (key.clone(), text.clone(), *retire)
                        })
                        .collect(),
                )
            }
            _ => None,
        }
        })
        .expect("the line's opening input")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_line_is_told_its_own_parent_at_open() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let outer = open_host_line(&session, "main", "outer line").await;
    move_main(&session).await;
    let (_, text, _) = settle_line(&session, &outer).await;
    assert_eq!(
        parent_block_line(&text),
        "main",
        "the outer line hears main"
    );
    let inner = open_host_line(&session, &outer, "inner line").await;
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == inner)
        .and_then(|info| info.branch_point)
        .expect("the inner line's cut");

    // 1. The opening input: the identity, then the block naming the outer line.
    let events = local_events(&session, &inner).await;
    let opening = opening_blocks(&events);
    assert_eq!(
        opening.len(),
        2,
        "the opening input carries the identity and one parent block: {opening:?}"
    );
    assert!(
        opening[0].1.starts_with("<dim-branch-line"),
        "the identity comes first: {opening:?}"
    );
    assert_eq!(
        opening[1].0.as_deref(),
        Some("branch:parent"),
        "the block stays in effect: {opening:?}"
    );
    assert!(!opening[1].2, "the block is live, not retired: {opening:?}");
    let (head, after) = parent_block(&opening[1].1);
    assert_eq!(
        (head, after),
        (cut, cut),
        "the block reports the cut: {opening:?}"
    );
    assert_eq!(
        parent_block_line(&opening[1].1),
        outer,
        "the block names the line that opened it: {opening:?}"
    );
    let opening_sequence = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|block| {
                matches!(block, InputBlock::Raw { key: Some(key), .. } if key == "branch:line")
            }))
        })
        .expect("the opening input event")
        .sequence;

    // 2. The line's latest live parent block names its own parent.
    let latest = parent_block_events(&events)
        .last()
        .cloned()
        .expect("the opening block is on the line");
    assert_eq!(parent_block_line(&latest.1), outer, "{latest:?}");

    // 3. A compaction replays that block, and never one naming main.
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session
            .submit(text, InputMode::Queue, &inner)
            .await
            .unwrap();
        patient(session.idle(&inner)).await.unwrap();
    }
    let message = baseline_message(&local_events(&session, &inner).await, "branch:parent")
        .expect("the line compacted and its parent block replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the parent block is text: {message:?}");
    };
    assert!(
        text.starts_with(&format!(
            "<dim-branch-parent branch=\"{outer}\" head=\"{cut}\" after=\"{cut}\">"
        )),
        "{text}"
    );
    assert!(
        !text.contains("branch=\"main\""),
        "the replayed block never names main: {text}"
    );

    // 4. When the outer line moves, the usual block follows, after the cut.
    patient(session.submit("OUTER-MOVES".into(), InputMode::Queue, &outer))
        .await
        .unwrap();
    patient(session.idle(&outer)).await.unwrap();
    let blocks = second_parent_block(&session, &inner, opening_sequence).await;
    let moved = blocks.last().expect("a block after the opening one");
    assert_eq!(
        parent_block_line(&moved.1),
        outer,
        "the block names the line that opened it: {blocks:?}"
    );
    assert_eq!(
        parent_block(&moved.1).1,
        cut,
        "the read cursor is still the cut: {blocks:?}"
    );

    // 5. A line cut from main gets no such block.
    let plain = open_host_line(&session, "main", "plain line").await;
    let plain_events = local_events(&session, &plain).await;
    let plain_opening = opening_blocks(&plain_events);
    assert_eq!(
        plain_opening.len(),
        1,
        "a line cut from main carries its identity alone: {plain_opening:?}"
    );
    assert!(
        plain_opening[0].1.starts_with("<dim-branch-line")
            && plain_opening[0].1.contains("source=\"host\""),
        "{plain_opening:?}"
    );
    assert!(
        blocks_with(&plain_events, "<dim-branch-parent").is_empty(),
        "no parent block is written or carried"
    );

    // 6. The reads still follow the chain.
    let (code, _) = call(&session, &inner, r#"{"action":"read","branch":"main"}"#).await;
    assert_eq!(code, "branch_not_kin", "the inner line cannot read main");
    let page = page_of(
        &tool_call(
            &session,
            &inner,
            &format!(r#"{{"action":"read","branch":"{outer}"}}"#),
        )
        .await,
    );
    assert!(
        !page_sequences(&page).is_empty(),
        "the inner line reads the line that opened it: {page}"
    );
    soon(agent.close()).await.unwrap();
}

/// A read that fails while a nested line opens fails that create: the caller
/// gets the error, the line the child was cut from records
/// `branch/create-failed` for it, and that line keeps dispatching, so a later
/// create under it opens
/// ([§7.1.1](../../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_line_whose_opening_read_fails_is_a_failed_create() {
    let store = Arc::new(TestStore::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let outer = open_host_line(&session, "main", "outer line").await;
    // The outer line hears main's move and writes the live parent block the
    // inner line's opening read looks for; the refusal then fails that read.
    move_main(&session).await;
    let (_, text, _) = settle_line(&session, &outer).await;
    assert_eq!(
        parent_block_line(&text),
        "main",
        "the outer line hears main"
    );
    store.arm(&outer);
    let at = session.sequence(&outer).unwrap();
    let created = tokio::time::timeout(
        Duration::from_secs(30),
        session.branch(outer.clone(), at, options("inner line")),
    )
    .await
    .expect("a hang guard: the failed read must not leave the create waiting");
    assert!(
        created.is_err(),
        "the create reports the failed read: {created:?}"
    );
    let events = until_events(&session, &outer, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)))
    })
    .await;
    let failed: Vec<&BranchCreateFailedData> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreateFailed(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "one failed create is recorded: {failed:?}");
    let BranchCreateFailedData::Failed { branch, .. } = failed[0] else {
        panic!("the read failure fails the create: {failed:?}");
    };
    assert!(!branch.is_empty(), "{failed:?}");

    store.disarm();
    let inner = open_host_line(&session, &outer, "inner line").await;
    let opening = opening_blocks(&local_events(&session, &inner).await);
    assert_eq!(
        opening.len(),
        2,
        "the later create under the same line opens with its parent block: {opening:?}"
    );
    assert_eq!(
        parent_block_line(&opening[1].1),
        outer,
        "the block names the line that opened it: {opening:?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_writes_a_due_parent_block_once_and_none_without_movement() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("watch the tests").source,
                branch: "line".into(),
                prompt: "watch the tests".into(),
            })
            .into(),
        )
        .unwrap();
    let moved = moved_turn(&main, "moved");
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "line".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the tests",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
        .await
        .unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    let events = until_events(&session, "line", |events| {
        !parent_block_events(events).is_empty()
    })
    .await;
    let blocks = parent_block_events(&events);
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let (head, after) = parent_block(&blocks[0].1);
    assert!(
        head >= moved.sequence,
        "the due block names the parent's head: {blocks:?}"
    );
    assert_eq!(after, create.sequence, "the due block names the line's cut");
    soon(agent.close()).await.unwrap();

    // The same saved state, but the line's last block already reports the
    // parent's head: a restore writes nothing.
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("watch the tests").source,
                branch: "line".into(),
                prompt: "watch the tests".into(),
            })
            .into(),
        )
        .unwrap();
    let moved = moved_turn(&main, "moved");
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "line".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    let opened = child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the tests",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: opened.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let text = dim_agent_capabilities::context_block::element(
        "dim-branch-parent",
        &[
            ("branch", "main"),
            ("head", &moved.sequence.to_string()),
            ("after", &create.sequence.to_string()),
        ],
        "Line main has new activity.",
    );
    child
        .append(
            EventKind::InputMessage(InputMessageData {
                blocks: vec![dim_agent_capabilities::context_block::raw(
                    text,
                    Some("branch:parent".into()),
                )],
                mode: Some(InputMode::Append),
                ..InputMessageData::default()
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let agent = MinimalHost::start(&builder_with_store(&probe, store))
        .await
        .unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    until_branch(&session, "line").await;
    soon(session.idle("line")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = local_events(&session, "line").await;
    assert_eq!(
        parent_block_events(&events).len(),
        1,
        "no block is due past the head it reports"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ending_line_and_main_get_no_parent_block() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("watch the tests").source,
                branch: "line".into(),
                prompt: "watch the tests".into(),
            })
            .into(),
        )
        .unwrap();
    moved_turn(&main, "moved");
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "line".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "work on the tests",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: options("watch the tests").source,
                ancestor: None,
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store))
        .await
        .unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    until_branch(&session, "main").await;
    soon(session.idle("main")).await.unwrap();
    until_branch(&session, "line").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = local_events(&session, "line").await;
    assert!(
        parent_block_events(&events).is_empty(),
        "an ending line gets no block: {events:#?}"
    );
    let events = local_events(&session, "main").await;
    assert!(parent_block_events(&events).is_empty(), "main gets none");
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_parent_block_replays_after_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "compact my line").await;
    quiet(&session, &child).await;
    move_main(&session).await;
    let (block, text, _) = settle_line(&session, &child).await;
    let (head, after) = parent_block(&text);
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session
            .submit(text, InputMode::Queue, &child)
            .await
            .unwrap();
        patient(session.idle(&child)).await.unwrap();
    }
    let message = baseline_message(&local_events(&session, &child).await, "branch:parent")
        .expect("the line compacted and its parent block replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the parent block is text: {message:?}");
    };
    assert!(
        text.starts_with(&format!(
            "<dim-branch-parent branch=\"main\" head=\"{head}\" after=\"{after}\">"
        )),
        "{text}"
    );
    let events = local_events(&session, &child).await;
    assert!(block <= events.last().unwrap().sequence);
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_creating_turn_alone_does_not_tell_the_line() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    soon(session.idle(&child)).await.unwrap();
    // Stands in for the parent-block writer handling the creating turn's end.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = local_events(&session, &child).await;
    assert!(
        parent_block_events(&events).is_empty(),
        "the turn that opened the line is not activity: {events:#?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::InputConsumed(_)))
            .count(),
        1,
        "the line's purpose turn consumes one input"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_tool_call_in_the_creating_turn_tells_the_line() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_model(&probe, Arc::new(ActingModel)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let events = until_events(&session, &child, |events| {
        !parent_block_events(events).is_empty()
    })
    .await;
    let blocks = parent_block_events(&events);
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let (head, after) = parent_block(&blocks[0].1);
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .branch_point
        .unwrap();
    assert_eq!(after, cut);
    let main_events = local_events(&session, "main").await;
    assert!(
        main_events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolUse(data) if data.call.name == "echo")
                && event.sequence > cut
                && event.sequence <= head
        }),
        "a tool call after the creating call is activity: {main_events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_message_answered_with_text_tells_the_line_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    let call = serde_json::json!({
        "action": "message",
        "to": "main",
        "content": "how is it going?",
        "mode": "steer"
    })
    .to_string();
    let result = tool_call(&session, &child, &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    soon(session.idle("main")).await.unwrap();
    soon(session.idle(&child)).await.unwrap();
    // Stands in for the parent-block writer handling the creating turn's end.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = local_events(&session, &child).await;
    assert!(
        parent_block_events(&events).is_empty(),
        "a line's message and a text answer are not activity: {events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_is_told_when_its_parent_makes_a_tool_call() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_model(&probe, Arc::new(ActingModel)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "watch the tests").await;
    quiet(&session, &line).await;
    let at = session.sequence("main").unwrap();
    let host = soon(session.branch("main".into(), at, options("host line")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == host)
        .unwrap()
        .branch_point
        .unwrap();
    // A cut at the parent's head becomes the create event's own sequence.
    assert!(cut >= at);
    // A host line cannot message main, so a tool line's steer is the movement:
    // it reaches main as a block, and the echo call main answers with is the
    // only activity after the host line's cut.
    let call = serde_json::json!({
        "action": "message",
        "to": "main",
        "content": "go on",
        "mode": "steer"
    })
    .to_string();
    let result = tool_call(&session, &line, &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    let events = until_events(&session, &host, |events| {
        !parent_block_events(events).is_empty()
    })
    .await;
    let blocks = parent_block_events(&events);
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let (head, after) = parent_block(&blocks[0].1);
    assert_eq!(after, cut);
    let main_events = local_events(&session, "main").await;
    assert!(
        main_events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolUse(data) if data.call.name == "echo")
                && event.sequence > cut
                && event.sequence <= head
        }),
        "main's tool call is activity for a host line: {main_events:#?}"
    );
    assert!(
        main_events.iter().all(|event| {
            event.sequence <= cut
                || !matches!(&event.kind, EventKind::InputMessage(data) if data.user.is_some())
        }),
        "no operator input after the cut: the tool call is the only activity: {main_events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purpose_tool_call_is_the_lines_first_step() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_model(&probe, Arc::new(PurposeModel)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "tool:echo {\"text\":\"purpose\"}").await;
    soon(session.idle("main")).await.unwrap();
    let events = until_events(&session, &child, |events| {
        events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolCall(data)
                if data.calls.iter().any(|call| call.name == "echo"))
        })
    })
    .await;
    assert!(
        parent_block_events(&events).is_empty(),
        "the turn that opened the line is not activity: {events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

/// A `main` that already ran an operator input and a tool call, with a tool
/// line that can steer it without operator input, then a host line opened at
/// `at` (0, or the end of a first turn). Returns the agent, the session, the
/// host line, the tool line and the cut.
async fn a_host_line_opened_after_main_moved(
    probe: &Arc<Probe>,
    older_cut: bool,
) -> (MinimalHost, Session, String, String, u64) {
    let agent = setup(probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let at = if older_cut {
        soon(session.submit("hello".into(), InputMode::Queue, "main"))
            .await
            .unwrap();
        soon(session.idle("main")).await.unwrap();
        session.sequence("main").unwrap()
    } else {
        0
    };
    soon(session.submit(
        "tool:echo {\"text\":\"before\"}".into(),
        InputMode::Queue,
        "main",
    ))
    .await
    .unwrap();
    soon(session.idle("main")).await.unwrap();
    let tool = open_line(&session, "watch the tests").await;
    quiet(&session, &tool).await;
    let host = soon(session.branch("main".into(), at, options("watch the tests")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    (agent, session, host, tool, at)
}

/// A message from a tool line is a block, not operator input: it moves `main`
/// without making the turn's end news.
async fn steer_main_from(session: &Session, line: &str) {
    let call = serde_json::json!({
        "action": "message",
        "to": "main",
        "content": "go on",
        "mode": "steer"
    })
    .to_string();
    let result = tool_call(session, line, &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_cut_at_the_start_is_not_told_what_happened_before_it() {
    let probe = Arc::new(Probe::default());
    let (agent, session, host, tool, _) = a_host_line_opened_after_main_moved(&probe, false).await;
    steer_main_from(&session, &tool).await;
    quiet(&session, &host).await;
    // Stands in for the parent-block writer handling the parent's turn end.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = local_events(&session, &host).await;
    assert!(
        parent_block_events(&events).is_empty(),
        "what main did before the line existed is not news: {events:#?}"
    );
    let first = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request.messages),
            _ => None,
        })
        .expect("the line made a request");
    assert!(
        !first
            .iter()
            .any(|message| said(message).contains("<dim-branch-parent")),
        "the first request carries only the opening inputs"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_cut_at_the_start_is_told_when_main_acts_after_it() {
    let probe = Arc::new(Probe::default());
    let (agent, session, host, tool, _) = a_host_line_opened_after_main_moved(&probe, false).await;
    steer_main_from(&session, &tool).await;
    quiet(&session, &host).await;
    soon(session.submit(
        "tool:echo {\"text\":\"later\"}".into(),
        InputMode::Queue,
        "main",
    ))
    .await
    .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = until_events(&session, &host, |events| {
        !parent_block_events(events).is_empty()
    })
    .await;
    let blocks = parent_block_events(&events);
    let (_, text) = blocks.last().expect("a parent block");
    let (head, after) = parent_block(text);
    assert_eq!(after, 0, "a first read starts at the cut");
    let main_events = local_events(&session, "main").await;
    assert!(
        main_events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolUse(data) if data.call.name == "echo")
                && event.sequence <= head
        }),
        "main's later tool call is news: {main_events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_cut_before_it_was_opened_is_not_told_about_the_gap() {
    let probe = Arc::new(Probe::default());
    let (agent, session, host, tool, at) = a_host_line_opened_after_main_moved(&probe, true).await;
    let opened = session
        .branches()
        .into_iter()
        .find(|info| info.id == host)
        .unwrap()
        .create_anchor
        .expect("a creation anchor");
    let main_events = local_events(&session, "main").await;
    assert!(
        main_events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolUse(_))
                && event.sequence > at
                && event.sequence < opened
        }),
        "the gap holds a tool call the block must not call news: {main_events:#?}"
    );
    steer_main_from(&session, &tool).await;
    quiet(&session, &host).await;
    // Stands in for the parent-block writer handling the parent's turn end.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = local_events(&session, &host).await;
    assert!(
        parent_block_events(&events).is_empty(),
        "the gap between the cut and the opening is not news: {events:#?}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_creating_call_is_written_before_the_line_was_opened() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .expect("the line");
    let opened = info.create_anchor.expect("a creation anchor");
    let Some(BranchSource::Tool { call_id, .. }) = info.source else {
        panic!("a tool line");
    };
    let main_events = local_events(&session, "main").await;
    let call = main_events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::ToolUse(data) if data.call.id == call_id))
        .expect("the creating call");
    assert!(
        call.sequence < opened,
        "the call that created the line precedes the line's creation event"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_line_tells_main_once_when_its_turn_ends_without_a_word() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let events = until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-idle").is_empty()
    })
    .await;
    let notices = blocks_with(&events, "<dim-branch-idle");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with(&format!(
        "<dim-branch-idle branch=\"{child}\" status=\"completed\">"
    )));
    assert!(notices[0].contains(&format!(
        "Line {child}'s turn ended (completed) without a message to you, and {child} is waiting."
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::InputMessage(data)
        if data.effective_mode() == InputMode::Steer
            && data.blocks.iter().any(|block| matches!(block, InputBlock::Raw { text, .. }
                if text.starts_with("<dim-branch-idle")))))
    );
    soon(session.idle("main")).await.unwrap();
    session
        .submit(
            r#"tool:wait {"millis":600}"#.into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    session
        .submit("behind the wait".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    until_events(&session, &child, |events| {
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .count()
            >= 2
    })
    .await;
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-idle").len() >= 2
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len(),
        2
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_line_without_idle_notices_stays_quiet_after_restore() {
    let store = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id().to_owned();
    let result = tool_call(
        &session,
        "main",
        r#"{"action":"create","prompt":"watch the tests","idleNotice":"none"}"#,
    )
    .await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    let child = session
        .branches()
        .into_iter()
        .find(|info| info.id != "main")
        .expect("the create result names a published line")
        .id;
    until_events(&session, &child, |events| {
        events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TurnEnd(data)
                    if matches!(&data.outcome, TurnOutcome::Completed { .. })
            )
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").is_empty());
    soon(agent.close()).await.unwrap();

    let agent = MinimalHost::start(&builder_with_store(&probe, store))
        .await
        .unwrap();
    let session = soon(agent.restore_session(id)).await.unwrap();
    soon(session.idle(&child)).await.unwrap();
    let before = local_events(&session, &child)
        .await
        .last()
        .unwrap()
        .sequence;
    session
        .submit("Check again".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    until_events(&session, &child, |events| {
        events.iter().any(|event| {
            event.sequence > before
                && matches!(
                    &event.kind,
                    EventKind::TurnEnd(data)
                        if matches!(&data.outcome, TurnOutcome::Completed { .. })
                )
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").is_empty());

    let result = tool_call(
        &session,
        &child,
        r#"{"action":"message","to":"main","content":"A test failed","mode":"steer"}"#,
    )
    .await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-message")
            .iter()
            .any(|text| text.contains("A test failed"))
    })
    .await;
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_that_steered_main_writes_no_idle_notice() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-idle").is_empty()
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    let before = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    let call =
        r#"{"action":"message","to":"main","content":"the leak is in cache.rs","mode":"steer"}"#
            .to_string();
    let result = tool_call(&session, &child, &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    until_events(&session, "main", |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)))
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    assert_eq!(
        after, before,
        "a turn that steered main writes no idle notice"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_with_a_held_steer_still_tells_main_the_line_stopped() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-idle").is_empty()
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    session
        .submit(
            r#"tool:wait {"millis":3000}"#.into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    until_events(&session, &child, |events| {
        events.iter().any(
            |event| matches!(&event.kind, EventKind::ToolUse(data) if data.call.name == "wait"),
        )
    })
    .await;
    let steer = session
        .submit("also look at the logs".into(), InputMode::Steer, &child)
        .await
        .unwrap();
    until_events(&session, &child, |events| {
        events.iter().any(
            |event| {
                matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == steer.id)
            },
        )
    })
    .await;
    soon(session.stop(&child, "user stop".into()))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-idle")
            .iter()
            .any(|notice| notice.contains(r#"status="cancelled""#))
    })
    .await;
    let events = local_events(&session, &child).await;
    assert!(
        !events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::InputConsumed(data) if data.input_id == steer.id
            )
        }),
        "the stop leaves the steer unconsumed"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_line_ends_itself_and_main_learns_it_from_the_roster() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let result = tool_call(
        &session,
        &child,
        r#"{"action":"end","content":"done: the leak is fixed"}"#,
    )
    .await;
    assert_eq!(
        result.content,
        ToolContent::Text(format!(
            "Line {child} is ending; your final message was sent to main."
        ))
    );
    let events = until_events(&session, "main", |events| {
        events.iter().any(
            |event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == child),
        )
    })
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data)
        if data.branch == child
            && matches!(data.source, BranchSource::Tool { .. })
            && data.ancestor.is_none()))
    );
    let finals = blocks_with(
        &events,
        &format!("<dim-branch-message from=\"{child}\" to=\"main\" final=\"true\">"),
    );
    assert_eq!(finals.len(), 1, "{finals:?}");
    assert!(finals[0].contains("done: the leak is fixed"));
    assert!(finals[0].ends_with(&format!(
        "\n\nLine {child} ends with this message.\n</dim-branch-message>"
    )));
    assert!(blocks_with(&events, "<dim-branch-ended").is_empty());
    let retired =
        "<dim-branch-lines branch=\"main\">\nAll lines you opened have ended.\n</dim-branch-lines>"
            .to_string();
    until_events(&session, "main", |events| {
        roster_block(events) == Some((retired.clone(), true))
    })
    .await;
    let child_events = local_events(&session, &child).await;
    assert!(
        child_events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnd(data)
        if matches!(data.source, BranchSource::Tool { .. }) && data.ancestor.is_none()))
    );
    assert!(branch_events(&session, &child, ..).await.is_err());
    assert_eq!(
        call(
            &session,
            "main",
            &format!(r#"{{"action":"message","to":"{child}","content":"still there?"}}"#)
        )
        .await,
        (
            "branch_ended".into(),
            format!("Line {child} has ended. Read it to see what it did.")
        )
    );
    let page = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"read","branch":"{child}"}}"#),
    )
    .await;
    assert_eq!(page.is_error, None, "{:?}", page.content);
    let page = page.structured_content.unwrap();
    assert_eq!(page["done"], serde_json::json!(true));
    let items = page["items"].as_array().expect("the page lists items");
    assert!(
        items
            .iter()
            .any(|item| item["type"] == serde_json::json!("turn_end"))
    );
    let cursor = page["nextCursor"].as_u64().expect("a cursor");
    assert!(
        items
            .iter()
            .all(|item| item["sequence"].as_u64().unwrap() <= cursor),
        "nextCursor follows every item returned"
    );
    let after = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"read","branch":"{child}","after":{cursor}}}"#),
    )
    .await;
    assert_eq!(after.is_error, None, "{:?}", after.content);
    let after = after.structured_content.unwrap();
    assert_eq!(after["items"], serde_json::json!([]));
    assert_eq!(after["done"], serde_json::json!(true));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_end_in_the_same_turn_writes_no_second_final_message() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let before = tool_results(&session, &child).await.len();
    session
        .submit(
            r#"double:branch {"action":"end","content":"done: first"} | branch {"action":"end","content":"done: second"}"#
                .into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    let results = soon(async {
        loop {
            let results: Vec<ToolResult> = local_events(&session, &child)
                .await
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
                    _ => None,
                })
                .collect();
            if results.len() >= before + 2 {
                return results;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let pair = &results[before..before + 2];
    assert_eq!(
        pair[0].content,
        ToolContent::Text(format!(
            "Line {child} is ending; your final message was sent to main."
        ))
    );
    assert_eq!(pair[1].is_error, Some(true), "{:?}", pair[1].content);
    let structured = pair[1].structured_content.clone().unwrap_or_default();
    assert_eq!(
        structured.get("error"),
        Some(&serde_json::json!("branch_ended"))
    );
    let events = until_events(&session, "main", |events| {
        events.iter().any(
            |event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == child),
        )
    })
    .await;
    let finals = blocks_with(
        &events,
        &format!("<dim-branch-message from=\"{child}\" to=\"main\" final=\"true\">"),
    );
    assert_eq!(finals.len(), 1, "{finals:?}");
    assert!(finals[0].contains("done: first"));
    assert!(!finals[0].contains("done: second"));
    assert!(blocks_with(&events, "<dim-branch-ended").is_empty());
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_lines_end_says_nothing_to_main() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let host = soon(session.branch("main".into(), 0, options("host line")))
        .await
        .unwrap();
    soon(session.idle(&host)).await.unwrap();
    assert!(roster_block(&local_events(&session, "main").await).is_none());
    soon(session.end_branch(
        &host,
        BranchSource::Host {
            operation_id: "test-end".into(),
        },
    ))
    .await
    .unwrap();
    let events = until_events(&session, "main", |events| {
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == host))
    })
    .await;
    assert!(blocks_with(&events, "<dim-branch-ended").is_empty());
    assert!(roster_block(&events).is_none());
    let tree = session.session_service::<TreeCommands>().unwrap();
    let facts = local_events(&session, &host).await;
    let head = facts.last().unwrap().sequence;
    let mut query = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        include_publishing: true,
        from: None,
        through: Some(head),
        query: QuerySpec {
            filter: None,
            select: QuerySelect::Fields {
                fields: vec!["id".into()],
            },
            group_by: vec![],
            pick: None,
            order_by: vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction: TimelineQueryDirection::Descending,
            }],
        },
        page: Some(QueryPage {
            limit: std::num::NonZeroU64::new(2).unwrap(),
            after: None,
        }),
    };
    let first = tree.query(host.clone(), query.clone()).await.unwrap();
    assert_eq!(first.through, head);
    let QueryOutput::Rows { rows } = first.result else {
        panic!("expected property rows")
    };
    assert_eq!(
        rows[0].values[0],
        QueryValue::Value {
            value: serde_json::json!(facts.last().unwrap().id)
        }
    );
    assert_eq!(
        rows[1].values[0],
        QueryValue::Value {
            value: serde_json::json!(facts[facts.len() - 2].id)
        }
    );
    query.page.as_mut().unwrap().after = first.next;
    let second = tree.query(host.clone(), query.clone()).await.unwrap();
    let QueryOutput::Rows { rows } = second.result else {
        panic!("expected property rows")
    };
    assert_eq!(
        rows[0].values[0],
        QueryValue::Value {
            value: serde_json::json!(facts[facts.len() - 3].id)
        }
    );
    query.through = None;
    assert!(matches!(
        tree.query(host.clone(), query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    query.through = Some(head);
    query.page.as_mut().unwrap().after = None;
    query.scope = TimelineQueryScope::Chain;
    assert!(matches!(
        tree.query(host.clone(), query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    query.scope = TimelineQueryScope::Branch;
    query.through = Some(head + 1);
    assert!(matches!(
        tree.query(host.clone(), query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    query.through = Some(head);
    query.from = Some(TimelinePosition {
        branch_id: "main".into(),
        sequence: 1,
    });
    assert!(matches!(
        tree.query(host, query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    query.from = None;
    query.through = None;
    assert!(matches!(
        session.query("main", query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    let main = tree.get("main").unwrap();
    assert!(matches!(
        main.query(query.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
    query.include_publishing = false;
    let via_session = session.query("main", query.clone()).await.unwrap();
    query.through = Some(via_session.through);
    let via_branch = main.query(query).await.unwrap();
    assert_eq!(
        serde_json::to_value(via_session).unwrap(),
        serde_json::to_value(via_branch).unwrap()
    );
    soon(agent.close()).await.unwrap();
}

/// The tree's page never cuts an atomic batch: `through` must end one, for an
/// open line and for an ended one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tree_page_refuses_a_through_inside_a_batch() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "read the batch").await;
    soon(session.idle(&child)).await.unwrap();
    let timeline = probe.child_timeline.lock().unwrap().clone().unwrap();
    let input = |text: &str| {
        EventKind::InputMessage(InputMessageData::text(text, Some(InputMode::Append))).into()
    };
    let batch = timeline
        .append_batch_registered(vec![input("first"), input("second")], |_| Ok(()))
        .unwrap();
    let (inside, end) = (batch[0].sequence, batch[1].sequence);
    let commands = session.session_service::<TreeCommands>().unwrap();
    let refused = |page: Result<Vec<Arc<AgentEvent>>>, ended: bool| {
        assert!(
            matches!(page, Err(AgentError::InvalidInput(ref message)) if message.contains("inside an atomic batch")),
            "an interior through is refused (ended: {ended})"
        );
    };
    refused(commands.page(child.clone(), 0, 8, inside).await, false);
    soon(session.end_branch(
        &child,
        BranchSource::Host {
            operation_id: "test-end".into(),
        },
    ))
    .await
    .unwrap();
    refused(commands.page(child.clone(), 0, 8, inside).await, true);
    let page = commands.page(child, 0, 1024, end).await.unwrap();
    assert_eq!(page.last().map(|event| event.sequence), Some(end));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_from_outside_a_tool_line_wakes_main_with_a_notice() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let source = BranchSource::Host {
        operation_id: "test-end".into(),
    };
    soon(session.end_branch(&child, source.clone()))
        .await
        .unwrap();
    let events = until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-ended").is_empty()
    })
    .await;
    let notices = blocks_with(&events, "<dim-branch-ended");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with(&format!("<dim-branch-ended branch=\"{child}\">")));
    assert!(
        notices[0].contains("It no longer receives messages; read it if you need what it did.")
    );
    let input = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
            if data.blocks.iter().any(|block| matches!(block, InputBlock::Raw { text, .. }
                if text.starts_with("<dim-branch-ended"))))
        })
        .unwrap();
    let EventKind::InputMessage(data) = &input.kind else {
        unreachable!()
    };
    assert_eq!(data.effective_mode(), InputMode::Steer);
    assert!(data.blocks.iter().any(|block| matches!(block,
        InputBlock::Raw { key: Some(key), retire: true, .. } if key == "branch:roster")));
    let child_events = local_events(&session, &child).await;
    assert!(
        child_events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnd(data)
        if data.source == source && data.ancestor.is_none()))
    );
    soon(agent.close()).await.unwrap();
}

/// Starts Wait's `wait` in a turn on `main` and answers its call id and
/// sequence once the call is written and no result has landed, so a notice
/// written after this reaches a wait that is still waiting.
async fn start_wait(session: &Session) -> (String, u64) {
    soon(session.submit(
        r#"tool:wait {"timeoutMs":3000}"#.into(),
        InputMode::Queue,
        "main",
    ))
    .await
    .unwrap();
    let events = until_events(session, "main", |events| {
        events.iter().any(
            |event| matches!(&event.kind, EventKind::ToolUse(data) if data.call.name == "wait"),
        )
    })
    .await;
    let (sequence, call) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolUse(data) if data.call.name == "wait" => {
                Some((event.sequence, data.call.id.clone()))
            }
            _ => None,
        })
        .expect("the wait call");
    assert!(
        !events.iter().any(
            |event| event.sequence > sequence && matches!(event.kind, EventKind::ToolResult(_))
        ),
        "the wait is still waiting"
    );
    (call, sequence)
}

/// The text of the result the wait call `call` ended with on `main`.
async fn wait_ended(session: &Session, call: &str) -> String {
    let events = until_events(session, "main", |events| {
        events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolResult(data)
                if data.results.iter().any(|result| result.call_id == call))
        })
    })
    .await;
    let result = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data
                .results
                .iter()
                .find(|result| result.call_id == call)
                .map(|result| result.result.clone()),
            _ => None,
        })
        .expect("the wait result");
    let ToolContent::Text(text) = result.content else {
        panic!("a wait result is text: {result:?}")
    };
    text
}

/// The notice block at or after `after` on `main`, as a steer, and the first
/// request after it that carries it.
async fn notice_in_a_request(session: &Session, after: u64, marker: &str) -> String {
    let events = until_events(session, "main", |events| {
        events.iter().any(|event| {
            event.sequence > after
                && matches!(&event.kind, EventKind::InputMessage(data)
                    if data.effective_mode() == InputMode::Steer
                        && data.blocks.iter().any(|block| matches!(block, InputBlock::Raw { text, .. }
                            if text.starts_with(marker))))
        })
    })
    .await;
    let (sequence, notice) = events
        .iter()
        .filter(|event| event.sequence > after)
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| match block {
                InputBlock::Raw { text, .. } if text.starts_with(marker) => {
                    Some((event.sequence, text.clone()))
                }
                _ => None,
            }),
            _ => None,
        })
        .expect("the notice was written as a steer");
    until_events(session, "main", |events| {
        events.iter().any(|event| {
            event.sequence > sequence
                && matches!(&event.kind, EventKind::ModelCall(data)
                    if data.request.messages.iter().any(|message| said(message).contains(&notice)))
        })
    })
    .await;
    notice
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_line_wakes_the_wait_of_the_main_that_opened_it() {
    let probe = Arc::new(Probe::default());
    let gates = Arc::new(GateModel::default());
    let agent = MinimalHost::start(&builder_with_wait(&probe, gates.clone()))
        .await
        .unwrap();
    let session = patient(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    patient(session.idle("main")).await.unwrap();
    patient(session.submit("GATE-LINE".into(), InputMode::Queue, &child))
        .await
        .unwrap();
    patient(gates.line_entered.notified()).await;
    let mut subscribed = session.service::<Wait>("main").unwrap().subscription();
    let (call, at) = start_wait(&session).await;
    patient(subscribed.subscribed()).await;
    gates.line_release.notify_one();
    assert_eq!(wait_ended(&session, &call).await, "wait ended: steer");
    let notice = notice_in_a_request(&session, at, "<dim-branch-idle").await;
    assert!(
        notice.starts_with(&format!(
            "<dim-branch-idle branch=\"{child}\" status=\"completed\">"
        )),
        "{notice}"
    );
    patient(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_line_wakes_the_wait_of_the_main_that_opened_it() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_wait(&probe, Arc::new(FlowModel)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let mut subscribed = session.service::<Wait>("main").unwrap().subscription();
    let (call, at) = start_wait(&session).await;
    soon(subscribed.subscribed()).await;
    soon(session.end_branch(
        &child,
        BranchSource::Host {
            operation_id: "test-end".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(wait_ended(&session, &call).await, "wait ended: steer");
    let notice = notice_in_a_request(&session, at, "<dim-branch-ended").await;
    assert!(
        notice.starts_with(&format!("<dim-branch-ended branch=\"{child}\">")),
        "{notice}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_message_from_a_line_wakes_the_wait_of_the_main_that_opened_it() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_wait(&probe, Arc::new(FlowModel)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();
    let mut subscribed = session.service::<Wait>("main").unwrap().subscription();
    let (call, at) = start_wait(&session).await;
    soon(subscribed.subscribed()).await;
    let sent = tool_call(
        &session,
        &child,
        r#"{"action":"message","to":"main","content":"the leak is in cache.rs","mode":"steer"}"#,
    )
    .await;
    assert_eq!(sent.is_error, None, "{:?}", sent.content);
    assert_eq!(wait_ended(&session, &call).await, "wait ended: steer");
    let notice = notice_in_a_request(&session, at, "<dim-branch-message").await;
    assert!(
        notice.starts_with(&format!(
            "<dim-branch-message from=\"{child}\" to=\"main\">"
        )),
        "{notice}"
    );
    assert!(notice.contains("the leak is in cache.rs"), "{notice}");
    soon(agent.close()).await.unwrap();
}

#[derive(Clone, Copy)]
struct Compact;
impl ResolveContextModel for Compact {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async { Ok(Arc::new(Compact) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for Compact {
    fn context_window(&self) -> f64 {
        4096.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        2000.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
}
fn diagnostics() -> StrategyDiagnostics {
    StrategyDiagnostics {
        model_usage: None,
        model_call_count: 1,
        record: CompactionStrategyRecord {
            kind: "test".into(),
            details: JsonObject::new(),
        },
    }
}
impl CompactionStrategy for Compact {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new(
            "",
            "",
            vec![dim_agent_capabilities::history::summary_message("short")],
        ))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            match input.cut() {
                Some(cut) => Ok(StrategyOutcome::Completed {
                    retain_from: cut.retain_from.clone(),
                    replacement: vec![dim_agent_capabilities::history::summary_message("short")],
                    diagnostics: diagnostics(),
                }),
                None => Ok(StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: diagnostics(),
                }),
            }
        })
    }
}
fn baseline_message(events: &[Arc<AgentEvent>], key: &str) -> Option<Message> {
    let baseline = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::ContextCompaction(ContextCompactionData {
            outcome:
                ContextCompactionResult::Completed {
                    context_baseline, ..
                },
            ..
        }) => Some(context_baseline.clone()),
        _ => None,
    })?;
    baseline
        .into_iter()
        .find(|entry| entry.key == key)
        .map(|entry| std::sync::Arc::unwrap_or_clone(entry.message))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_roster_and_an_identity_replay_separately_after_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let first = open_line(&session, "compact my line").await;
    wait_for_roster(&session, &first).await;
    let second = open_line(&session, "compact me too").await;
    compact_line(&session, "main").await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let message =
        baseline_message(&events, "branch:roster").expect("main compacted and the roster replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the roster is text: {message:?}");
    };
    assert!(
        text.starts_with("<dim-branch-lines branch=\"main\">"),
        "{text}"
    );
    assert!(text.contains(&format!("- {first}: compact my line")));
    assert!(text.contains(&format!("- {second}: compact me too")));
    assert!(
        baseline_message(&events, "branch:line").is_none(),
        "the root keeps no identity under the roster's key"
    );
    compact_line(&session, &second).await;
    let events = chain_events(&session, &second, ..).await.unwrap();
    let message = baseline_message(&events, "branch:line")
        .expect("the child compacted and its identity replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the identity is text: {message:?}");
    };
    assert!(
        text.starts_with(&format!(
            "<dim-branch-line branch=\"{second}\" parent=\"main\" source=\"tool\">"
        )),
        "{text}"
    );
    assert!(
        baseline_message(&events, "branch:roster").is_none(),
        "the inherited roster was retired at the child's open and never replays"
    );
    soon(agent.close()).await.unwrap();
}

/// The compaction above and the child's replay need a line cut after its
/// opener wrote a roster, so the inherited block is in the child's prefix.
async fn wait_for_roster(session: &Session, line: &str) {
    let events = until_events(session, "main", |events| {
        blocks_with(events, "<dim-branch-lines")
            .last()
            .is_some_and(|text| text.contains(line))
    })
    .await;
    let roster = roster_block(&events).expect("the roster is keyed `branch:roster`");
    assert!(!roster.1, "the live roster is not retired: {roster:?}");
}
/// Two turns long enough to make the line compact.
async fn compact_line(session: &Session, line: &str) {
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session.submit(text, InputMode::Queue, line).await.unwrap();
        patient(session.idle(line)).await.unwrap();
    }
}

/// Case 1a: a child cut after its opener wrote a roster inherits that block,
/// and its opening input retires it; a child cut before the roster gets no
/// such block, and its opening input is what it was. The retirement lives
/// under the roster's key alone, so a status note on the same line leaves it
/// standing (design §7.1, §20.1a).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_cut_after_a_roster_retires_it_and_one_cut_before_gets_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let before = open_line(&session, "cut before the roster").await;
    let opening = opening_blocks(&local_events(&session, &before).await);
    assert_eq!(
        opening.len(),
        1,
        "a line cut before the roster carries its identity alone: {opening:?}"
    );
    assert_eq!(opening[0].0.as_deref(), Some("branch:line"));
    wait_for_roster(&session, &before).await;
    let after = open_line(&session, "cut after the roster").await;
    let opening = opening_blocks(&local_events(&session, &after).await);
    assert_eq!(
        opening.len(),
        2,
        "the identity, then the block retiring the inherited roster: {opening:?}"
    );
    assert_eq!(opening[0].0.as_deref(), Some("branch:line"));
    assert_eq!(opening[1].0.as_deref(), Some("branch:roster"));
    assert!(opening[1].2, "the inherited roster is retired: {opening:?}");
    assert_eq!(
        opening[1].1,
        format!(
            "<dim-branch-lines branch=\"{after}\">\nThe list above belongs to line main and names the lines it opened. This line has opened none of its own, so it keeps no list.\n</dim-branch-lines>"
        ),
        "{opening:?}"
    );
    let rosters = keyed_blocks(&local_events(&session, &after).await, "branch:roster");
    assert_eq!(
        rosters,
        vec![(opening[1].1.clone(), true)],
        "the line writes no roster of its own"
    );

    soon(session.set_default_branch(
        &after,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let noted = until_events(&session, &after, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    let note = line_block(&noted).expect("the note replaces the identity");
    assert!(note.0.contains(r#"default="true""#), "{:?}", note.0);
    assert_eq!(
        keyed_blocks(&local_events(&session, &after).await, "branch:roster"),
        rosters,
        "the note is written under the identity's key and leaves the roster's block alone"
    );
    soon(agent.close()).await.unwrap();
}

type TreeDispatch = (
    Arc<dim_agent_capabilities::timeline_tree::TimelineTree>,
    Arc<Dispatcher>,
    Ctx,
);
struct CaptureTreeDispatch {
    ports: Arc<Mutex<Option<TreeDispatch>>>,
    tree: Key<dim_agent_capabilities::timeline_tree::TimelineTree>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
}
impl CaptureTreeDispatch {
    fn new(names: &Names<'_>, ports: Arc<Mutex<Option<TreeDispatch>>>) -> rutis::Result<Self> {
        Ok(Self {
            ports,
            tree: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for CaptureTreeDispatch {
    fn name(&self) -> &str {
        "test.capture-tree-dispatch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.tree.erase(),
            self.dispatcher.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            if identity.branch_id == "main" {
                *self.ports.lock().unwrap() = Some((tree, dispatcher, ctx.clone()));
            }
            Ok(Effect::Done)
        })
    }
}

/// The handler is installed after readiness, and the current-thread runtime is
/// required so the spawned build is not polled before the callback.
#[tokio::test]
async fn closing_after_dispatch_before_the_build_first_poll_is_a_normal_cancellation() {
    let ports = Arc::new(Mutex::new(None));
    let captured = ports.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.require_feature(
        Feature::new("test.capture-tree-dispatch")
            .branch("test.capture-tree-dispatch", move |names| {
                Ok(Box::new(CaptureTreeDispatch::new(names, captured.clone())?))
            }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let (tree, dispatcher, root) = ports.lock().unwrap().take().unwrap();
    dispatcher
        .subscribe(
            &root,
            "close after create dispatch",
            &["branch/create"],
            move |_| {
                tree.begin_close();
                Box::pin(async { Ok(()) })
            },
        )
        .unwrap();
    assert!(
        soon(session.branch("main".into(), 0, options("cancelled")))
            .await
            .is_err()
    );
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
}

fn interrupt() -> EventKind {
    EventKind::StopInterrupt(StopInterruptData {
        reason: "batch filler".into(),
        scope: InterruptScope::Turn,
    })
}

/// A stop does not wait for a branch the `branch` tool already created: the
/// tool call settles as cancelled and the turn ends, while the tree still
/// carries the build and settles the creation itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_settles_the_branch_tool_while_the_tree_still_builds_the_child() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    probe.delay_child.store(true, Ordering::SeqCst);
    session
        .submit(
            r#"tool:branch {"action":"create","prompt":"child"}"#.into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    soon(probe.child_entered.notified()).await;
    soon(session.stop("main", "user stop".into()))
        .await
        .unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let result = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        result.is_error,
        Some(true),
        "the stopped call is an error: {:?}",
        result.content
    );
    assert_eq!(
        result.content,
        ToolContent::Text("stopped waiting; the branch creation continues".into())
    );
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Cancelled { .. }))));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)))
    );
    probe.child_release.notify_one();
    soon(async {
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            if events
                .iter()
                .any(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    soon(agent.close()).await.unwrap();
}

#[derive(Default)]
struct Gate {
    target: Mutex<Option<String>>,
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}
struct GatePlugin {
    gate: Arc<Gate>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
}
impl DynPlugin for GatePlugin {
    fn name(&self) -> &str {
        "test.gate"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let gate = self.gate.clone();
            let branch = identity.branch_id.clone();
            dispatcher.subscribe(ctx, "gate", &["branch/end"], move |_event| {
                let gate = gate.clone();
                let branch = branch.clone();
                Box::pin(async move {
                    let mine = gate.target.lock().unwrap().as_deref() == Some(branch.as_str());
                    if mine && gate.armed.swap(false, Ordering::SeqCst) {
                        gate.entered.notify_one();
                        gate.release.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}
fn gated(probe: &Arc<Probe>, gate: &Arc<Gate>) -> AgentBuilder {
    let mut builder = builder(probe);
    let gate = gate.clone();
    builder.require_feature(Feature::new("test.gate").branch("test.gate", move |names| {
        Ok(Box::new(GatePlugin {
            gate: gate.clone(),
            dispatcher: names.key()?,
            identity: names.key()?,
        }))
    }));
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ancestors_end_closes_a_pending_end_whose_delivery_was_held() {
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());
    let agent = MinimalHost::start(&gated(&probe, &gate)).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let ancestor = soon(session.branch("main".into(), 0, options("ancestor")))
        .await
        .unwrap();
    soon(session.idle(&ancestor)).await.unwrap();
    let child = soon(session.branch(
        ancestor.clone(),
        0,
        BranchOptions {
            prompt: "tool line".into(),
            source: BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    *gate.target.lock().unwrap() = Some(child.clone());
    gate.armed.store(true, Ordering::SeqCst);
    session
        .submit(
            r#"double:branch {"action":"end","content":"the work is done"} | wait {"millis":20000}"#
                .into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    until_events(&session, &child, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_)))
    })
    .await;
    let ender = {
        let session = session.clone();
        let ancestor = ancestor.clone();
        tokio::spawn(async move {
            session
                .end_branch(
                    &ancestor,
                    BranchSource::Host {
                        operation_id: "op:end".into(),
                    },
                )
                .await
        })
    };
    // Stands in for the ancestor's end reaching the child's held end: its own
    // commit only proves `end()` was polled, and the cascade that waits for
    // the child runs on a task spawned from that commit's delivery.
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(8), ender)
        .await
        .expect("the ancestor's end waited for the line's own turn")
        .unwrap()
        .unwrap();
    assert!(
        session.ended_head(&child).is_some(),
        "the line is recorded as ended"
    );
    let events = local_events(&session, &child).await;
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Cancelled { .. }))));
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnd(data) => Some((event.sequence, data)),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 1, "the committed end is not replaced");
    let results: Vec<_> = local_events(&session, &ancestor)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == child => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [BranchEndedData {
            branch: child,
            end_event: ends[0].0,
            source: ends[0].1.source.clone(),
            ancestor: ends[0].1.ancestor.clone(),
        }],
        "cancellation preserves the line's committed end and records its result once"
    );
    soon(agent.close()).await.unwrap();
}

async fn copy_session(
    from: &MemoryTimelineStore,
    to: &MemoryTimelineStore,
    session: &str,
    rewrite: impl Fn(&str, Vec<StoredEvent>) -> Vec<StoredEvent>,
) {
    to.create_session(session, &SessionConfig::default())
        .await
        .unwrap();
    let mut branches = from.branches(session).await.unwrap();
    let depth = |id: &str, all: &[StoredBranch]| {
        let mut depth = 0;
        let mut at = all.iter().find(|branch| branch.info.id == id).unwrap();
        while let Some(parent) = &at.info.parent {
            depth += 1;
            at = all.iter().find(|branch| &branch.info.id == parent).unwrap();
        }
        depth
    };
    let snapshot = branches.clone();
    branches.sort_by_key(|branch| depth(&branch.info.id, &snapshot));
    for branch in branches {
        if branch.sequence == 0 {
            continue;
        }
        let key = TimelineKey {
            session_id: session.into(),
            branch_id: branch.info.id.clone(),
        };
        let records = from.read(&key, 1..=branch.sequence).await.unwrap();
        let records = rewrite(&branch.info.id, records);
        if records.is_empty() {
            continue;
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        to.append(
            key,
            records,
            Box::new(move |result| {
                let _ = sender.send(result);
            }),
        )
        .unwrap();
        receiver.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_spanning_a_restart_after_a_steer_writes_no_idle_notice() {
    let first = Arc::new(MemoryTimelineStore::default());
    let probe = Arc::new(Probe::default());
    let mut builder1 = builder(&probe);
    builder1.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(first.clone()));
    let agent = MinimalHost::start(&builder1).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id().to_owned();
    let child = open_line(&session, "watch the tests").await;
    until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-idle").is_empty()
    })
    .await;
    soon(session.idle("main")).await.unwrap();
    let call =
        r#"{"action":"message","to":"main","content":"the leak is in cache.rs","mode":"steer"}"#;
    let result = tool_call(&session, &child, call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    soon(session.idle("main")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    soon(agent.close()).await.unwrap();

    let second = Arc::new(MemoryTimelineStore::default());
    let line = child.clone();
    copy_session(&first, &second, &id, |branch, mut records| {
        if branch != line {
            return records;
        }
        let at = records
            .iter()
            .rposition(|record| matches!(&record.event.kind, EventKind::ToolResult(_)))
            .expect("the steer's result");
        let end = records[at].batch_end as usize;
        records.truncate(end);
        records
    })
    .await;
    let probe = Arc::new(Probe::default());
    let mut builder2 = builder(&probe);
    builder2.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(second));
    let agent = MinimalHost::start(&builder2).await.unwrap();
    let session = soon(agent.restore_session(id)).await.unwrap();
    soon(session.idle(&child)).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let child_events = local_events(&session, &child).await;
    let restart_end = child_events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(format!("{:?}", data.outcome)),
            _ => None,
        });
    let after = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    assert!(
        restart_end.is_some(),
        "the restart ends the turn that was open"
    );
    assert_eq!(
        after, before,
        "a turn that steered main before the restart writes no idle notice"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_chain_queries_preserve_inherited_cuts_after_ending_and_restore() {
    async fn texts(tree: &TreeCommands, branch: &str) -> Vec<String> {
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Chain,
            from: None,
            through: None,
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::Prefix {
                    value: QueryOperand::Field {
                        path: "data.user.content[0].text".into(),
                    },
                    prefix: "visible-".into(),
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data.user.content[0].text".into()],
                },
                group_by: vec![],
                pick: None,
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: std::num::NonZeroU64::MIN,
                after: None,
            }),
        };
        let mut texts = Vec::new();
        loop {
            let reply = tree.query(branch.into(), query.clone()).await.unwrap();
            query.through = Some(reply.through);
            let QueryOutput::Rows { rows } = reply.result else {
                panic!("property rows")
            };
            for row in rows {
                let QueryValue::Value { value } = &row.values[0] else {
                    panic!("input text")
                };
                texts.push(value.as_str().unwrap().to_owned());
            }
            if reply.next.is_none() {
                break;
            }
            query.page.as_mut().unwrap().after = reply.next;
        }
        query.include_publishing = true;
        assert!(matches!(
            tree.query(branch.into(), query).await,
            Err(AgentError::InvalidInput(_))
        ));
        texts
    }
    let probe = Arc::new(Probe::default());
    let store = Arc::new(MemoryTimelineStore::default());
    let config = builder_with_store(&probe, store);
    let agent = MinimalHost::start(&config).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("visible-root".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let child = session
        .branch("main".into(), cut, options("child"))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    session
        .submit("visible-child".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let leaf = session
        .branch(
            child.clone(),
            session.sequence(&child).unwrap(),
            options("leaf"),
        )
        .await
        .unwrap();
    let zero = session
        .branch(child.clone(), 0, options("zero cut"))
        .await
        .unwrap();
    soon(session.idle(&leaf)).await.unwrap();
    soon(session.idle(&zero)).await.unwrap();
    for branch in ["main", child.as_str()] {
        session
            .submit(
                "visible-excluded-after-cut".into(),
                InputMode::Queue,
                branch,
            )
            .await
            .unwrap();
        soon(session.idle(branch)).await.unwrap();
    }
    session
        .submit("visible-leaf".into(), InputMode::Queue, &leaf)
        .await
        .unwrap();
    soon(session.idle(&leaf)).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let expected = ["visible-root", "visible-child", "visible-leaf"];
    assert_eq!(texts(&tree, &leaf).await, expected);
    assert_eq!(texts(&tree, &zero).await, ["visible-root"]);
    session
        .end_branch(
            &leaf,
            BranchSource::Host {
                operation_id: "test-end".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(texts(&tree, &leaf).await, expected);
    let session_id = session.id().to_owned();
    soon(agent.close()).await.unwrap();
    let agent = MinimalHost::start(&config).await.unwrap();
    let restored = agent.restore_session(session_id).await.unwrap();
    let tree = restored.session_service::<TreeCommands>().unwrap();
    assert_eq!(texts(&tree, &leaf).await, expected);
    assert_eq!(texts(&tree, &zero).await, ["visible-root"]);
    soon(agent.close()).await.unwrap();
}

/// A chain read pages on the queried branch, so a page that ends in an
/// ancestor continues at that fact rather than restarting on the branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_read_continues_past_a_page_that_ended_in_an_ancestor() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let head = session.sequence("main").unwrap();
    let child = soon(session.branch("main".into(), head, options("child line")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let whole = chain_events(&session, &child, ..).await.unwrap();
    assert!(
        whole.len() >= 2,
        "the chain holds the ancestor and the branch: {}",
        whole.len()
    );
    for page in 1..=whole.len() as u64 {
        let paged = chain_events_paged(&session, &child, .., page)
            .await
            .unwrap();
        assert_eq!(
            paged.iter().map(|event| &event.id).collect::<Vec<_>>(),
            whole.iter().map(|event| &event.id).collect::<Vec<_>>(),
            "a page of {page} reads the same chain"
        );
    }
    let end = 3;
    assert!(
        head > end,
        "the ancestor reaches past the range end: {head}"
    );
    let bounded = chain_events(&session, &child, ..=end).await.unwrap();
    let opening = bounded
        .iter()
        .rposition(|event| matches!(event.kind, EventKind::BranchStart(_)))
        .expect("the chain holds the branch's opening");
    assert_eq!(
        bounded[opening..]
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=end).collect::<Vec<_>>(),
        "the range holds the branch's first facts"
    );
    for page in 1..=bounded.len() as u64 {
        let paged = chain_events_paged(&session, &child, ..=end, page)
            .await
            .unwrap();
        assert_eq!(
            paged.iter().map(|event| &event.id).collect::<Vec<_>>(),
            bounded.iter().map(|event| &event.id).collect::<Vec<_>>(),
            "a page of {page} reads the chain to the range end"
        );
    }
    soon(agent.close()).await.unwrap();
}

/// #1165: the tree's directory lists open lines and ended lines, `main`
/// first, and an ended line's `endEvent` is its own `branch/end`, cascaded
/// ends included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_directory_lists_open_and_cascade_ended_lines_with_their_end_events() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("ancestor".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence("main").unwrap();
    let parent = soon(session.branch("main".into(), cut, options("parent")))
        .await
        .unwrap();
    soon(session.idle(&parent)).await.unwrap();
    let grandchild = soon(session.branch(parent.clone(), 0, options("grandchild")))
        .await
        .unwrap();
    soon(session.idle(&grandchild)).await.unwrap();

    let directory = session.directory();
    assert_eq!(
        directory.first().map(|entry| entry.info.id.as_str()),
        Some("main"),
        "the directory starts with main"
    );
    let open = directory
        .iter()
        .find(|entry| entry.info.id == parent)
        .expect("the parent is listed while open");
    assert_eq!(open.state, BranchDirectoryState::Open);
    assert_eq!(open.end_event, None);
    assert_eq!(open.head, session.sequence(&parent).unwrap());
    let child = directory
        .iter()
        .find(|entry| entry.info.id == grandchild)
        .expect("the grandchild is listed while open");
    assert_eq!(child.info.parent.as_deref(), Some(parent.as_str()));

    let source = BranchSource::Host {
        operation_id: "op:directory".into(),
    };
    soon(session.end_branch(&parent, source.clone()))
        .await
        .unwrap();

    let own_end = |events: Vec<Arc<AgentEvent>>| -> u64 {
        events
            .iter()
            .find_map(|event| {
                matches!(event.kind, EventKind::BranchEnd(_)).then_some(event.sequence)
            })
            .expect("the line holds its own end")
    };
    let parent_end = own_end(local_events(&session, &parent).await);
    let grandchild_end = own_end(local_events(&session, &grandchild).await);

    let directory = session.directory();
    assert_eq!(
        directory.first().map(|entry| entry.info.id.as_str()),
        Some("main"),
        "main stays first after the end"
    );
    assert_eq!(
        directory[0].state,
        BranchDirectoryState::Open,
        "main cannot end"
    );
    let ended = directory
        .iter()
        .find(|entry| entry.info.id == parent)
        .expect("the ended parent is listed");
    assert_eq!(ended.state, BranchDirectoryState::Ended);
    assert_eq!(ended.end_event, Some(parent_end));
    assert_eq!(ended.head, session.sequence(&parent).unwrap());
    let cascaded = directory
        .iter()
        .find(|entry| entry.info.id == grandchild)
        .expect("the cascaded grandchild is listed");
    assert_eq!(cascaded.state, BranchDirectoryState::Ended);
    assert_eq!(cascaded.end_event, Some(grandchild_end));
    assert_eq!(cascaded.head, session.sequence(&grandchild).unwrap());
    soon(agent.close()).await.unwrap();
}

/// #1165: a tool-sourced end commits while the turn that wrote it still runs;
/// the directory reports the line as ending with its committed `branch/end`
/// and a live head, then as ended with the same event and a fixed head.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_directory_reports_a_line_whose_end_is_pending_as_ending() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let ancestor = soon(session.branch("main".into(), 0, options("ancestor")))
        .await
        .unwrap();
    soon(session.idle(&ancestor)).await.unwrap();
    let child = soon(session.branch(
        ancestor.clone(),
        0,
        BranchOptions {
            prompt: "tool line".into(),
            source: BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    session
        .submit(
            r#"double:branch {"action":"end","content":"the work is done"} | wait {"millis":20000}"#
                .into(),
            InputMode::Queue,
            &child,
        )
        .await
        .unwrap();
    soon(async {
        loop {
            if tool_results(&session, &child)
                .await
                .iter()
                .any(|result| matches!(&result.content, ToolContent::Text(text) if text.contains("is ending")))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let end = local_events(&session, &child)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(_) => Some(event.sequence),
            _ => None,
        })
        .expect("the line ended itself");
    let directory = session.directory();
    let ending = directory
        .iter()
        .find(|entry| entry.info.id == child)
        .expect("the ending line is listed");
    assert_eq!(
        ending.state,
        BranchDirectoryState::Ending,
        "the end is committed and the turn still runs"
    );
    assert_eq!(
        ending.end_event,
        Some(end),
        "the pending end carries its own branch/end"
    );
    assert!(
        ending.head >= end,
        "the ending head includes the committed end"
    );
    assert!(
        ending.head <= session.sequence(&child).unwrap(),
        "the ending head is a live value"
    );
    assert_eq!(
        directory
            .iter()
            .find(|entry| entry.info.id == ancestor)
            .expect("the ancestor is listed")
            .state,
        BranchDirectoryState::Open,
        "the ancestor is untouched"
    );

    soon(session.end_branch(
        &ancestor,
        BranchSource::Host {
            operation_id: "op:end".into(),
        },
    ))
    .await
    .unwrap();
    let directory = session.directory();
    let ended = directory
        .iter()
        .find(|entry| entry.info.id == child)
        .expect("the ended line is listed");
    assert_eq!(
        ended.state,
        BranchDirectoryState::Ended,
        "the turn ended and the line closed"
    );
    assert_eq!(
        ended.end_event,
        Some(end),
        "the same branch/end is the result"
    );
    assert_eq!(ended.head, session.sequence(&child).unwrap());
    assert!(
        ended.head > ending.head,
        "the line published past the head it had while ending"
    );
    soon(agent.close()).await.unwrap();
}

/// #1165: a host end commits `branch/end` and closes the line in one flow.
/// While the close is held, the directory reports the line as ending with its
/// own end event and a live head, before its ended row exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_directory_reports_a_host_end_as_ending_before_its_ended_row() {
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());
    let agent = MinimalHost::start(&gated(&probe, &gate)).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = soon(session.branch("main".into(), 0, options("host line")))
        .await
        .unwrap();
    soon(session.idle(&line)).await.unwrap();
    *gate.target.lock().unwrap() = Some(line.clone());
    gate.armed.store(true, Ordering::SeqCst);
    let ender = {
        let session = session.clone();
        let line = line.clone();
        tokio::spawn(async move {
            session
                .end_branch(
                    &line,
                    BranchSource::Host {
                        operation_id: "op:end".into(),
                    },
                )
                .await
        })
    };
    soon(gate.entered.notified()).await;
    let end = local_events(&session, &line)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(_) => Some(event.sequence),
            _ => None,
        })
        .expect("the host end committed on the line");
    assert!(
        session.ended_head(&line).is_none(),
        "the ended row is still ahead while the close is held"
    );
    let directory = session.directory();
    let ending = directory
        .iter()
        .find(|entry| entry.info.id == line)
        .expect("the ending line is listed");
    assert_eq!(
        ending.state,
        BranchDirectoryState::Ending,
        "the host end committed and the close is held"
    );
    assert_eq!(
        ending.end_event,
        Some(end),
        "the ending line carries its own branch/end"
    );
    assert!(
        ending.head >= end && ending.head <= session.sequence(&line).unwrap(),
        "the ending head is live and includes the committed end"
    );

    gate.release.notify_one();
    patient(ender).await.unwrap().unwrap();
    let directory = session.directory();
    let ended = directory
        .iter()
        .find(|entry| entry.info.id == line)
        .expect("the ended line is listed");
    assert_eq!(ended.state, BranchDirectoryState::Ended);
    assert_eq!(
        ended.end_event,
        Some(end),
        "the same branch/end is the result"
    );
    assert_eq!(ended.head, session.sequence(&line).unwrap());
    soon(agent.close()).await.unwrap();
}

/// #1165: a cascade end commits a descendant's own `branch/end`. While that
/// descendant's close is held, the directory reports it as ending with its own
/// end event, before its ended row exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_directory_reports_a_cascaded_end_as_ending_before_its_ended_row() {
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());
    let agent = MinimalHost::start(&gated(&probe, &gate)).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let ancestor = soon(session.branch("main".into(), 0, options("ancestor")))
        .await
        .unwrap();
    soon(session.idle(&ancestor)).await.unwrap();
    let child = soon(session.branch(ancestor.clone(), 0, options("cascaded child")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    *gate.target.lock().unwrap() = Some(child.clone());
    gate.armed.store(true, Ordering::SeqCst);
    let ender = {
        let session = session.clone();
        let ancestor = ancestor.clone();
        tokio::spawn(async move {
            session
                .end_branch(
                    &ancestor,
                    BranchSource::Host {
                        operation_id: "op:end".into(),
                    },
                )
                .await
        })
    };
    soon(gate.entered.notified()).await;
    let end = local_events(&session, &child)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnd(_) => Some(event.sequence),
            _ => None,
        })
        .expect("the cascade committed the child's own branch/end");
    assert!(
        session.ended_head(&child).is_none(),
        "the child's ended row is still ahead while the close is held"
    );
    assert!(
        !local_events(&session, &ancestor)
            .await
            .iter()
            .any(|event| matches!(&event.kind,
                EventKind::BranchEnded(data) if data.branch == child)),
        "the child's result is not recorded yet"
    );
    let directory = session.directory();
    let ending = directory
        .iter()
        .find(|entry| entry.info.id == child)
        .expect("the ending child is listed");
    assert_eq!(
        ending.state,
        BranchDirectoryState::Ending,
        "the cascaded end committed and the child's close is held"
    );
    assert_eq!(
        ending.end_event,
        Some(end),
        "the cascaded child carries its own branch/end"
    );

    gate.release.notify_one();
    patient(ender).await.unwrap().unwrap();
    let directory = session.directory();
    let ended = directory
        .iter()
        .find(|entry| entry.info.id == child)
        .expect("the ended child is listed");
    assert_eq!(ended.state, BranchDirectoryState::Ended);
    assert_eq!(
        ended.end_event,
        Some(end),
        "the same branch/end is the result"
    );
    assert_eq!(ended.head, session.sequence(&child).unwrap());
    soon(agent.close()).await.unwrap();
}

#[tokio::test]
async fn timeline_store_binding_preserves_memory_batch_publication_and_wire_reads() {
    use dim_agent_runtime::{
        Feature, SessionConfig, Timeline, TimelineKey, TimelineStore, testing::MinimalHost,
    };
    use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
    use rutis_bridge::session::HostDispatch;
    use serde_json::json;
    use std::sync::{Arc, OnceLock};
    type Binding = (Arc<dyn TimelineStore>, Arc<dyn HostDispatch>);
    struct Probe {
        native: Key<dyn TimelineStore>,
        wire: Key<dyn HostDispatch>,
        binding: Arc<OnceLock<Binding>>,
    }
    impl DynPlugin for Probe {
        fn name(&self) -> &str {
            "TimelineStoreProbe"
        }
        fn injects(&self) -> Vec<TypeKey> {
            vec![self.native.erase(), self.wire.erase()]
        }
        fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
            Box::pin(async move {
                assert!(
                    self.binding
                        .set((ctx.require_at(&self.native)?, ctx.require_at(&self.wire)?))
                        .is_ok()
                );
                Ok(Effect::Done)
            })
        }
    }
    let binding = Arc::new(OnceLock::new());
    let capture = binding.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    defaults::install(&mut builder);
    builder.require_feature(Feature::new("TimelineStoreProbe").session(
        "TimelineStoreProbe",
        move |names| {
            Ok(Box::new(Probe {
                native: names.key()?,
                wire: names.named("timelineStore")?,
                binding: capture.clone(),
            }))
        },
    ));
    let host = MinimalHost::start(&builder).await.unwrap();
    host.create_session(SessionConfig::default()).await.unwrap();
    let (native, wire) = binding.get().unwrap();
    native
        .create_session("parity", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "parity".into(),
        branch_id: "main".into(),
    };
    let timeline = Timeline::new(host.root().clone(), key.clone(), vec![], native.clone());
    let inputs = ["one", "two"]
        .into_iter()
        .map(|text| {
            dim_agent_types::EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                text, None,
            ))
            .into()
        })
        .collect();
    let batch = timeline
        .append_batch_registered(inputs, |_| Ok(()))
        .unwrap();
    assert_eq!(
        timeline.sequence(),
        2,
        "Memory native publication remains synchronous"
    );
    let reply = wire
        .invoke(
            "readPage",
            json!([{"sessionId":"parity","branchId":"main","after":0,"limit":1,"through":2}])
                .into(),
        )
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap()
        .json()
        .unwrap();
    let native_records = native.read_page(&key, 0, 1, 2).await.unwrap();
    assert_eq!(
        reply["events"],
        serde_json::to_value(&native_records).unwrap()
    );
    assert_eq!(
        reply["events"].as_array().unwrap().len(),
        2,
        "the event target cannot split an atomic batch"
    );
    assert_eq!(reply["events"][1]["event"]["id"], batch[1].id);
    let segments = wire
        .invoke(
            "readSegments",
            json!([{"sessionId":"parity","segments":[
                {"branchId":"main","from":1,"through":2},
                {"branchId":"main","from":2,"through":2}
            ]}])
            .into(),
        )
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        segments["segments"],
        json!([
            native.read(&key, 1..=2).await.unwrap(),
            native.read(&key, 2..=2).await.unwrap(),
        ])
    );
    let boundary = wire
        .invoke(
            "batchEndAt",
            json!([{"sessionId":"parity","branchId":"main","sequence":1}]).into(),
        )
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(boundary, json!({"sequence":2}));
    let invalid = wire
        .invoke(
            "batchEnd",
            json!([{"sessionId":"parity","branchId":"main","sequence":1}]).into(),
        )
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), Some("invalid"));
    let rutis_bridge::session::Error::Remote { message, .. } = invalid else {
        panic!("expected an exported domain error")
    };
    assert_eq!(
        message,
        native.batch_end(&key, 1).await.unwrap_err().to_string()
    );
    assert!(
        wire.invoke("head", json!([["parity", "main"]]).into())
            .is_err()
    );
    let mut local = native_records[1].clone();
    let event = Arc::make_mut(&mut local.event);
    event.id = "local-wire-append".into();
    event.sequence = 3;
    local.batch_end = 3;
    let admitted = wire
        .invoke(
            "append",
            json!([{
                "sessionId":"parity","branchId":"main","batchId":"local-wire","events":[local]
            }])
            .into(),
        )
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(admitted, json!({"accepted":true}));
    assert_eq!(native.head(&key).await.unwrap(), 3);
    wire.invoke("releaseSession", json!([{"sessionId":"parity"}]).into())
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap();
    assert_eq!(native.head(&key).await.unwrap(), 3);
    wire.invoke("rollbackSession", json!([{"sessionId":"parity"}]).into())
        .unwrap()
        .reference()
        .unwrap()
        .wait_async()
        .await
        .unwrap();
    assert!(native.session_config("parity").await.unwrap().is_none());
    host.close().await.unwrap();
}

/// The tree's refusal code and message for a `TreeCommands` call.
fn refusal(error: &Result<()>) -> (&'static str, String) {
    match error {
        Err(AgentError::BranchRefused { refusal, message }) => (refusal.code(), message.clone()),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_branch_starts_at_the_root_and_survives_a_restart() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    assert_eq!(session.default_branch().unwrap(), "main");
    let line = open_line(&session, "a line to continue on").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    assert_eq!(
        tree.default_branch().unwrap(),
        "main",
        "a new line does not take the role"
    );
    let moved = soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(moved, line);
    assert_eq!(session.default_branch().unwrap(), line);
    let fact = local_events(&session, "main")
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::BranchDefault(data) => Some(data.clone()),
            _ => None,
        })
        .expect("the move is one fact on the root");
    assert_eq!(fact.branch, line);
    assert_eq!(fact.from, None);
    let id = session.id();
    soon(session.close()).await.unwrap();
    let session = soon(agent.restore_session(id.into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        session.default_branch().unwrap(),
        line,
        "the fact survives a restart"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_reads_one_fact_for_a_multi_line_session_and_none_for_a_single_line() {
    let single = Arc::new(TestStore::default());
    let agent = setup_with(&single).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
    let restored = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        single.clone(),
    ))
    .await
    .unwrap();
    let session = soon(restored.restore_session(id.into())).await.unwrap();
    assert_eq!(session.default_branch().unwrap(), "main");
    assert_eq!(
        single.pages("main", &["branch/default", "session/forked"]),
        0,
        "a Session with one line reads no fact"
    );
    soon(restored.close()).await.unwrap();

    let multi = Arc::new(TestStore::default());
    let agent = setup_with(&multi).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "one more line").await;
    soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let id = session.id();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
    let restored = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        multi.clone(),
    ))
    .await
    .unwrap();
    let session = soon(restored.restore_session(id.into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        multi.pages("main", &["branch/default", "session/forked"]),
        1,
        "a Session with more lines reads exactly one fact"
    );
    assert_eq!(session.default_branch().unwrap(), line);
    soon(restored.close()).await.unwrap();
}

/// #1862: a fork's `session/forked` names no branch, and a fork that opened
/// a line has more than one line, so restore reads that fact. The kind must
/// answer before the branch, or every restart of such a fork panics
/// (design §5, §20.20).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_of_a_fork_that_opened_a_line_keeps_the_root_default() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("forked", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let key = |branch: &str| TimelineKey {
        session_id: "forked".into(),
        branch_id: branch.into(),
    };
    let main = Timeline::new(root(), key("main"), vec![], store.clone());
    main.append(
        EventKind::BranchStart(BranchStartData {
            parent: None,
            create_event: None,
        })
        .into(),
    )
    .unwrap();
    let source = BranchSource::User {
        operation_id: "fork".into(),
    };
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: source.clone(),
                branch: "A".into(),
                prompt: "copied line".into(),
            })
            .into(),
        )
        .unwrap();
    let a = Timeline::new(
        root(),
        key("A"),
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    a.append(
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        })
        .into(),
    )
    .unwrap();
    a.append(
        EventKind::InputMessage(InputMessageData::text("work on a", Some(InputMode::Queue))).into(),
    )
    .unwrap();
    // A fork appends this after the copied facts; it carries no `branch`.
    main.append(EventKind::SessionForked(SessionForkedData {}).into())
        .unwrap();
    for timeline in [&a, &main] {
        dim_agent_runtime::core::close_timeline(timeline)
            .await
            .unwrap();
    }
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.restore_session("forked".into())).await.unwrap();
    assert_eq!(
        session.default_branch().unwrap(),
        "main",
        "a fork's default branch is its root, and its restore panics on no fact"
    );
    soon(agent.close()).await.unwrap();
}

/// #1862: an end whose final message fails to reach its parent must not be
/// finalized, or restore skips the ended line and the message is lost. The line
/// stays ending; the next restore delivers the message once under the id the
/// end derives, and a second restore writes nothing (design §10, §20.26b).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_whose_final_message_fails_is_delivered_by_the_next_restore() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let session_id = session.id().to_owned();
    let line = open_line(&session, "deliver me").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    store.fail_next_delivery("main");
    let failed = tree
        .end_with_final(
            line.clone(),
            BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
            Some("the result".into()),
        )
        .await;
    assert!(failed.is_err(), "the delivery failure reaches the caller");
    let entry = session
        .directory()
        .into_iter()
        .find(|entry| entry.info.id == line)
        .expect("the ending line is still listed");
    assert_eq!(entry.state, BranchDirectoryState::Ending);
    let end_event = entry.end_event.expect("the line's end is recorded");
    let submission = format!("branch-final:{line}:{end_event}");
    let deliveries = |events: &[Arc<AgentEvent>], submission: &str| {
        events
            .iter()
            .filter(|event| match &event.kind {
                EventKind::InputMessage(data) => data.submission_id.as_deref() == Some(submission),
                _ => false,
            })
            .count()
    };
    assert_eq!(
        deliveries(&local_events(&session, "main").await, &submission),
        0,
        "the failed delivery left nothing on the parent"
    );
    // A store that publishes an error faults the Session, so its close is the
    // point where the end has settled.
    let _ = soon(session.close()).await;
    assert!(
        session.ended_head(&line).is_none(),
        "an undelivered final message leaves the line ending"
    );
    let _ = soon(agent.close()).await;

    let restored = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(restored.restore_session(session_id.clone()))
        .await
        .unwrap();
    let events = local_events(&session, "main").await;
    assert_eq!(
        deliveries(&events, &submission),
        1,
        "the next restore delivers the final message once"
    );
    assert_eq!(
        events
            .iter()
            .filter(
                |event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == line)
            )
            .count(),
        1,
        "the line is recorded as ended once"
    );
    soon(session.close()).await.unwrap();
    soon(restored.close()).await.unwrap();

    let again = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(again.restore_session(session_id)).await.unwrap();
    assert_eq!(
        deliveries(&local_events(&session, "main").await, &submission),
        1,
        "a second restore writes no copy of the final message"
    );
    soon(session.close()).await.unwrap();
    soon(again.close()).await.unwrap();
}

/// #1862: a final message the store refused is a state of the line, not a
/// one-shot error. A later end answers the same failure while the store still
/// refuses and retries the delivery once it reads again, under the id
/// `branch/end` derives, so the message arrives once and the line is recorded
/// as ended (design §10, §20.26b).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_end_retries_an_undelivered_final_message() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "deliver me").await;
    soon(session.idle("main")).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let source = || BranchSource::Tool {
        turn_id: "turn".into(),
        request_id: "request".into(),
        call_id: "call".into(),
    };
    store.fail_next_lookup("main");
    let first = soon(tree.end_with_final(line.clone(), source(), Some("the result".into()))).await;
    assert!(
        first.is_err(),
        "the delivery failure reaches the caller: {first:?}"
    );
    let start = session
        .directory()
        .into_iter()
        .find(|entry| entry.info.id == line)
        .expect("the ending line is still listed");
    assert_eq!(start.state, BranchDirectoryState::Ending);
    let end_event = start.end_event.expect("the line's end is recorded");
    let submission = format!("branch-final:{line}:{end_event}");
    let deliveries = |events: &[Arc<AgentEvent>], submission: &str| {
        events
            .iter()
            .filter(|event| match &event.kind {
                EventKind::InputMessage(data) => data.submission_id.as_deref() == Some(submission),
                _ => false,
            })
            .count()
    };
    store.fail_next_lookup("main");
    let second = soon(tree.end_with_final(line.clone(), source(), Some("the result".into()))).await;
    let third = soon(tree.end_with_final(line.clone(), source(), Some("the result".into()))).await;
    // Bounded: a caller left waiting, or a line never recorded as ended, fails
    // here instead of stalling the suite.
    soon(async {
        while session.ended_head(&line).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        matches!(second, Err(AgentError::Storage(_))),
        "while the store still fails, the later end answers its error: {second:?}"
    );
    assert!(
        third.is_ok(),
        "the end that finds the store reading again answers: {third:?}"
    );
    assert_eq!(
        deliveries(&local_events(&session, "main").await, &submission),
        1,
        "the retry delivers the final message exactly once"
    );
    assert!(
        session.ended_head(&line).is_some(),
        "the line is recorded as ended"
    );
    // The refused deliveries stay in the tree's cleanup errors, which close
    // reports.
    let _ = soon(agent.close()).await;
}

/// #1862: ending a line whose descendant still owes a final message answers
/// instead of waiting. The cascade retries the descendant; because the parent
/// is ending, the delivery answers `branch_ended` and the descendant is
/// recorded as ended, then the ancestor's own end completes (design §10,
/// §20.26b).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_the_parent_of_a_line_with_an_undelivered_message_answers() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let parent = open_line(&session, "outer").await;
    let child = soon(session.branch(
        parent.clone(),
        0,
        BranchOptions {
            prompt: "inner".into(),
            source: BranchSource::User {
                operation_id: "nested".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    store.fail_next_lookup(&parent);
    let failed = soon(tree.end_with_final(
        child.clone(),
        BranchSource::Tool {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
        },
        Some("the inner result".into()),
    ))
    .await;
    assert!(
        failed.is_err(),
        "the delivery failure reaches the caller: {failed:?}"
    );
    assert_eq!(
        session.ended_head(&child),
        None,
        "the child is still ending"
    );
    let ended = soon(tree.end(
        parent.clone(),
        BranchSource::User {
            operation_id: "end".into(),
        },
    ))
    .await;
    assert!(ended.is_ok(), "ending the parent answers: {ended:?}");
    assert!(
        session.ended_head(&parent).is_some(),
        "the parent is recorded as ended"
    );
    assert!(
        session.ended_head(&child).is_some(),
        "the cascade retried the child and recorded its end"
    );
    let _ = soon(agent.close()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_the_root_the_default_branch_or_an_ancestor_is_refused() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "outer").await;
    let inner = soon(session.branch(
        line.clone(),
        0,
        BranchOptions {
            prompt: "inner".into(),
            source: BranchSource::User {
                operation_id: "nested".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&inner)).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let user = BranchSource::User {
        operation_id: "end".into(),
    };
    let root = tree.end("main".into(), user.clone()).await;
    assert!(
        matches!(&root, Err(AgentError::InvalidInput(message)) if message.contains("cannot end")),
        "the root's end is the input error its callers already receive: {root:?}"
    );
    soon(session.set_default_branch(
        &inner,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    for (target, code) in [
        (inner.as_str(), "branch_cannot_end"),
        (line.as_str(), "branch_cannot_end"),
    ] {
        let refused = tree.end(target.into(), user.clone()).await;
        assert_eq!(refusal(&refused).0, code, "{target}: {refused:?}");
    }
    assert!(
        tree.list()
            .iter()
            .any(|info| info.id == inner && info.parent.as_deref() == Some(line.as_str())),
        "a refused end leaves both lines open"
    );
    let write = local_events(&session, &inner).await;
    assert!(
        !write
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_))),
        "a refused end writes no end command"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_opened_line_records_its_own_source() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = soon(session.branch(
        "main".into(),
        0,
        BranchOptions {
            prompt: "the user opened this".into(),
            source: BranchSource::User {
                operation_id: "client-op".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == line)
        .unwrap();
    assert_eq!(
        info.source,
        Some(BranchSource::User {
            operation_id: "client-op".into()
        })
    );
    let opening = blocks_with(&local_events(&session, &line).await, "<dim-branch-line");
    assert_eq!(opening.len(), 1, "{opening:?}");
    assert!(
        opening[0].starts_with(&format!(
            "<dim-branch-line branch=\"{line}\" parent=\"main\" source=\"user\">"
        )),
        "{}",
        opening[0]
    );
    soon(agent.close()).await.unwrap();
}

/// A line the user opened reads the end rule it lives under: the line cannot
/// end itself and the user ends it, which no move changes, and the note that
/// makes it the default branch states the rule once (design §4, §12.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_opened_line_states_the_end_rule_that_never_changes() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = soon(session.branch(
        "main".into(),
        0,
        BranchOptions {
            prompt: "the user opened this".into(),
            source: BranchSource::User {
                operation_id: "client-op".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let opening = blocks_with(&local_events(&session, &line).await, "<dim-branch-line ");
    assert_eq!(opening.len(), 1, "{opening:?}");
    assert!(
        opening[0].contains("You cannot call end on yourself; the user ends this line."),
        "{}",
        opening[0]
    );
    assert!(
        !opening[0].contains("Unless the user makes this line the default branch"),
        "the birth block names no state in which the line may end itself: {}",
        opening[0]
    );
    switch(&session, &line).await;
    let note = owed_notes(&session, &line, 1).await.pop().unwrap();
    assert!(
        note.contains("you can open lines with create"),
        "the note states the right the move adds: {note}"
    );
    assert_eq!(
        note.matches("cannot call end on yourself").count(),
        1,
        "the note states the end rule once: {note}"
    );
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_default_branch_that_was_a_tool_line_stops_sending_its_idle_notice() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "work alone").await;
    // The opening turn ends without a word, so its idle notice is what a line
    // that is not the default branch sends; wait for it to land so the count
    // below is a baseline rather than a race.
    until_events(&session, "main", |events| {
        !blocks_with(events, "<dim-branch-idle").is_empty()
    })
    .await;
    soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let notices = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    assert!(notices > 0, "an idle line tells main when its turn ends");
    let ended = local_events(&session, &line)
        .await
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::TurnEnd(_)))
        .count();
    soon(session.submit("answer the user".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let turn_end = until_events(&session, &line, |events| {
        events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::TurnEnd(_)))
            .count()
            > ended
    })
    .await
    .last()
    .expect("the turn ended")
    .sequence;
    // The ordering point. The line's dispatcher runs its subscribers before it
    // reports the sequence, so reaching this `turn/end` means BranchLine folded
    // it and queued its idle decision; the parent block below is written by the
    // same writer later in that queue, so by the time it lands the idle decision
    // has been decided.
    let mut dispatched = session.dispatch_watch(&line).unwrap();
    while dispatched.borrow().sequence < turn_end {
        dispatched.changed().await.unwrap();
    }
    move_main(&session).await;
    until_events(&session, &line, |events| {
        parent_block_events(events)
            .last()
            .is_some_and(|(sequence, _)| *sequence > turn_end)
    })
    .await;
    assert_eq!(
        blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len(),
        notices,
        "the default branch sends no idle notice for the turn that ended at {turn_end}"
    );
    let (code, text) = call(&session, &line, r#"{"action":"end","content":"done"}"#).await;
    assert_eq!(code, "branch_cannot_end", "{text}");
    assert!(text.contains("default branch"), "{text}");
    soon(agent.close()).await.unwrap();
}

/// A note a move writes survives the line's next compaction: the tool line the
/// move makes the default branch, the tool line it displaced, a line the user
/// opened, and the root, which gets no note and so keeps no block with a
/// parent (design §5, §12.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switched_line_keeps_its_identity_after_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();

    // The tool line the first move makes the default branch, and the tool line
    // the next move displaces it with.
    let line = open_line(&session, "the parser race").await;
    let other = open_line(&session, "the renderer race").await;
    switch(&session, &line).await;
    compact_line(&session, &line).await;
    let note = line_block(&chain_events(&session, &line, ..).await.unwrap())
        .expect("the note is the latest block of the key");
    assert!(
        note.0.starts_with(&format!(
            "<dim-branch-line branch=\"{line}\" parent=\"main\" source=\"tool\" default=\"true\">"
        )),
        "{}",
        note.0
    );
    assert!(
        note.0.contains("Your purpose, as main wrote it:"),
        "{}",
        note.0
    );
    assert!(note.0.contains("the parser race"), "{}", note.0);
    assert!(
        note.0.contains("you cannot call end on yourself"),
        "the note's own end rule: {}",
        note.0
    );
    assert!(!note.1, "the note is live");

    // The tool line the next move displaces.
    switch(&session, &other).await;
    owed_notes(&session, &line, 2).await;
    compact_line(&session, &line).await;
    let note = line_block(&chain_events(&session, &line, ..).await.unwrap())
        .expect("the former default branch keeps the note");
    assert!(
        note.0.starts_with(&format!(
            "<dim-branch-line branch=\"{line}\" parent=\"main\" source=\"tool\" default=\"false\">"
        )),
        "{}",
        note.0
    );
    assert!(
        note.0.contains("Your purpose, as main wrote it:"),
        "{}",
        note.0
    );
    assert!(note.0.contains("the parser race"), "{}", note.0);
    assert!(
        note.0
            .contains(&format!("moved from this line to line {other}")),
        "the note names the line that holds the role: {}",
        note.0
    );
    assert!(
        note.0.contains(
            "Only a line opened with create can end itself, and not while it or a line below it is the default branch."
        ),
        "the note's own end rule: {}",
        note.0
    );
    assert!(!note.1, "the note is live");

    // A line the user opened, which the move makes the default branch.
    let cut = idle_cut(&session, "main").await;
    let opened = soon(session.branch(
        "main".into(),
        cut,
        BranchOptions {
            prompt: "the user line".into(),
            source: BranchSource::User {
                operation_id: "client".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&opened)).await.unwrap();
    switch(&session, &opened).await;
    owed_notes(&session, &opened, 1).await;
    compact_line(&session, &opened).await;
    let note = line_block(&chain_events(&session, &opened, ..).await.unwrap())
        .expect("the user line keeps the note");
    assert!(
        note.0.starts_with(&format!(
            "<dim-branch-line branch=\"{opened}\" parent=\"main\" source=\"user\" default=\"true\">"
        )),
        "{}",
        note.0
    );
    assert!(
        note.0.contains("opened by the user from line main"),
        "{}",
        note.0
    );
    assert!(
        note.0.contains("the user ends this line"),
        "the note's own end rule: {}",
        note.0
    );
    assert!(!note.0.contains("Your purpose, as"), "{}", note.0);
    assert!(!note.1, "the note is live");

    // The root: a move back to it writes no note, so its compaction replays no
    // identity block and the note it would carry names no parent.
    switch(&session, "main").await;
    compact_line(&session, "main").await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        baseline_message(&events, "branch:roster").is_some(),
        "the root compacted and replayed its roster"
    );
    assert!(
        baseline_message(&events, "branch:line").is_none(),
        "the root gets no note: its identity key stays empty"
    );
    assert!(
        line_block(&events).is_none(),
        "the root holds no `branch:line` block"
    );
    assert_eq!(session.default_branch().unwrap(), "main");
    soon(agent.close()).await.unwrap();
}

/// #1862: a tool line's purpose is the structured prompt its `branch/create`
/// recorded, so a move and a compaction restate every paragraph and the
/// escaped characters byte for byte (design §12.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_multi_paragraph_purpose_survives_a_move_and_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(
        &session,
        "the first paragraph\n\nand a second one with <tag> & text",
    )
    .await;
    soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let purpose = "Your purpose, as main wrote it:\n\
                   the first paragraph\n\n\
                   and a second one with &lt;tag&gt; &amp; text\n\n\
                   How you work with main:";
    let noted = until_events(&session, &line, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains("default=\"true\""))
    })
    .await;
    let note = line_block(&noted).expect("the note is the latest block of the key");
    assert!(
        note.0.contains(purpose),
        "the whole purpose survives: {}",
        note.0
    );
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session.submit(text, InputMode::Queue, &line).await.unwrap();
        patient(session.idle(&line)).await.unwrap();
    }
    let note = line_block(&chain_events(&session, &line, ..).await.unwrap())
        .expect("the note is still the latest block of the key");
    assert!(
        note.0.contains(purpose),
        "the whole purpose survives the compaction: {}",
        note.0
    );
    assert!(!note.1, "the note is live");
    soon(agent.close()).await.unwrap();
}

/// A move appends one note at the end of the line's own history, so everything
/// the line had before it is unchanged: that is what keeps the new default
/// branch's cached request prefix intact (design §7.3).
/// Whether the input carrying the latest `branch:line` block has its
/// `input/applied`, so a page read after it holds both the write and its
/// application.
fn identity_block_applied(events: &[Arc<AgentEvent>]) -> bool {
    let Some(at) = events.iter().rposition(|event| match &event.kind {
        EventKind::InputMessage(data) => data.blocks.iter().any(
            |block| matches!(block, InputBlock::Raw { key: Some(key), .. } if key == "branch:line"),
        ),
        _ => false,
    }) else {
        return false;
    };
    let id = &events[at].id;
    events
        .iter()
        .any(|event| matches!(&event.kind, EventKind::InputApplied(data) if &data.input_id == id))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_leaves_the_new_default_branchs_prefix_unchanged() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "keep this prefix").await;
    let inherited = chain_events(&session, &line, ..=0).await.unwrap();
    let before = local_events(&session, &line).await;
    let ids: Vec<&str> = before.iter().map(|event| event.id.as_str()).collect();
    soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let after = until_events(&session, &line, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains("default=\"true\""))
            && identity_block_applied(events)
    })
    .await;
    assert_eq!(
        chain_events(&session, &line, ..=0).await.unwrap(),
        inherited,
        "the inherited prefix the cache holds is untouched"
    );
    assert_eq!(
        after[..ids.len()]
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ids,
        "the line's own history before the move is unchanged"
    );
    let added: Vec<&str> = after[ids.len()..]
        .iter()
        .map(|event| event.kind.key())
        .collect();
    assert_eq!(
        added,
        ["input/message", "input/applied"],
        "the move appends the note and nothing else"
    );
    let note = line_block(&after).expect("the note is the latest block");
    assert!(note.0.contains("default=\"true\""), "{}", note.0);
    soon(agent.close()).await.unwrap();
}

/// E2: a read of the line a caller was cut from, at each depth of a chain,
/// before and after a compaction. Run with
/// `--release --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e2_chained_reads_before_and_after_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let mut chain = vec!["main".to_string()];
    for depth in 1..=3 {
        let parent = chain.last().cloned().unwrap();
        let line = soon(session.branch(parent, 0, options(&format!("depth {depth}"))))
            .await
            .unwrap();
        soon(session.idle(&line)).await.unwrap();
        soon(session.submit("work".into(), InputMode::Queue, &line))
            .await
            .unwrap();
        soon(session.idle(&line)).await.unwrap();
        chain.push(line);
    }
    async fn reads(session: &Session, chain: &[String]) -> Vec<(usize, Duration)> {
        let mut measured = Vec::new();
        for (depth, line) in chain.iter().enumerate().skip(1) {
            let parent = &chain[depth - 1];
            let mut samples = Vec::new();
            for _ in 0..5 {
                let started = std::time::Instant::now();
                let page = tool_call(
                    session,
                    line,
                    &format!(r#"{{"action":"read","branch":"{parent}"}}"#),
                )
                .await;
                assert_eq!(page.is_error, None, "{:?}", page.content);
                samples.push(started.elapsed());
            }
            samples.sort();
            measured.push((depth, samples[samples.len() / 2]));
        }
        measured
    }
    for (label, measured) in [
        ("before", reads(&session, &chain).await),
        ("after", {
            for line in &chain[1..] {
                compact_line(&session, line).await;
            }
            reads(&session, &chain).await
        }),
    ] {
        for (depth, elapsed) in measured {
            println!(
                "E2 chained read at depth {depth} {label} compaction: median {:.2} ms",
                elapsed.as_secs_f64() * 1e3
            );
        }
    }
    soon(agent.close()).await.unwrap();
}

/// E3: cold restore of a single-line and a multi-line Session (design §5,
/// §20.20). Run with `--release --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e3_cold_restore_single_and_multi_line() {
    for lines in [0, 3] {
        let mut elapsed = Vec::new();
        for _ in 0..5 {
            let probe = Arc::new(Probe::default());
            let agent = setup(&probe).await;
            let session = soon(agent.create_session(SessionConfig::default()))
                .await
                .unwrap();
            for line in 0..lines {
                let id = open_line(&session, &format!("line {line}")).await;
                if line == 1 {
                    soon(session.set_default_branch(
                        &id,
                        BranchSource::User {
                            operation_id: "switch".into(),
                        },
                    ))
                    .await
                    .unwrap();
                }
            }
            let id = session.id();
            soon(session.close()).await.unwrap();
            let started = std::time::Instant::now();
            let restored = soon(agent.restore_session(id.into())).await.unwrap();
            soon(restored.idle("main")).await.unwrap();
            elapsed.push(started.elapsed());
            soon(agent.close()).await.unwrap();
        }
        elapsed.sort();
        println!(
            "E3 cold restore with {lines} extra lines: median {:.1} ms",
            elapsed[elapsed.len() / 2].as_secs_f64() * 1e3
        );
    }
}

/// E4: a move on a warm Session, and the line it makes the default branch
/// (design §5, §14). Run with `--release --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e4_move_on_a_warm_session() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "warm target").await;
    let mut elapsed = Vec::new();
    for round in 0..10 {
        let target = if round % 2 == 0 { &line } else { "main" };
        let started = std::time::Instant::now();
        soon(session.set_default_branch(
            target,
            BranchSource::User {
                operation_id: format!("switch-{round}"),
            },
        ))
        .await
        .unwrap();
        elapsed.push(started.elapsed());
    }
    elapsed.sort();
    println!(
        "E4 move on a warm Session: median {:.2} ms",
        elapsed[elapsed.len() / 2].as_secs_f64() * 1e3
    );
    soon(agent.close()).await.unwrap();
}

/// The number of status notes each line holds, by the line they are on. A note
/// is a `branch:line` block with a `default` attribute; a birth identity has
/// none, so this counts only what a move wrote.
async fn default_notes(session: &Session, lines: &[&str]) -> Vec<(String, usize)> {
    let mut counted = Vec::new();
    for line in lines {
        let events = local_events(session, line).await;
        let notes = blocks_with(&events, "<dim-branch-line ")
            .iter()
            .filter(|text| text.contains(" default=\""))
            .count();
        counted.push(((*line).to_owned(), notes));
    }
    counted
}

/// §20.21: every participant of a move that is not the root receives exactly
/// one note, and the root receives none in either direction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_writes_one_note_per_participant_that_is_not_the_root() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    soon(session.idle("main")).await.unwrap();
    let switch = |line: String| {
        let session = session.clone();
        async move {
            soon(session.set_default_branch(
                &line,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            ))
            .await
            .unwrap()
        }
    };
    switch(b.clone()).await;
    until_events(&session, &b, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    assert_eq!(
        default_notes(&session, &["main", &b]).await,
        [("main".to_owned(), 0), (b.clone(), 1)],
        "main → B writes one note, to B"
    );
    let note = line_block(&local_events(&session, &b).await).expect("B holds its note");
    assert!(
        note.0.starts_with(&format!(
            "<dim-branch-line branch=\"{b}\" parent=\"main\" source=\"tool\" default=\"true\">"
        )),
        "{}",
        note.0
    );
    switch("main".into()).await;
    until_events(&session, &b, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="false""#))
    })
    .await;
    assert_eq!(
        default_notes(&session, &["main", &b]).await,
        [("main".to_owned(), 0), (b.clone(), 2)],
        "B → main writes one more note, to B, and none to the root"
    );
    let c = open_line(&session, "c").await;
    soon(session.idle("main")).await.unwrap();
    switch(b.clone()).await;
    until_events(&session, &b, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    switch(c.clone()).await;
    until_events(&session, &c, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    until_events(&session, &b, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="false""#))
    })
    .await;
    assert_eq!(
        default_notes(&session, &["main", &b, &c]).await,
        [("main".to_owned(), 0), (b.clone(), 4), (c.clone(), 1)],
        "B → C writes two notes, one per participant, and the root gets none"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.21, §12.1: two moves committed before a writer runs still earn one note
/// each, in commit order, and the line's next request carries its latest note.
/// The note test above waits for each note before the next move; this one does
/// not. One worker thread, so both moves commit before the writer is scheduled:
/// that is the order that left a line with no note at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn moves_that_overtake_their_writer_each_reach_their_participants() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    soon(session.idle("main")).await.unwrap();
    let switch = |line: String| {
        let session = session.clone();
        async move {
            soon(session.set_default_branch(
                &line,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            ))
            .await
            .unwrap()
        }
    };
    fn notes(events: &[Arc<AgentEvent>]) -> Vec<String> {
        blocks_with(events, "<dim-branch-line ")
            .into_iter()
            .filter(|text| text.contains(" default=\""))
            .collect()
    }
    switch(b.clone()).await;
    switch("main".into()).await;
    let events = until_events(&session, &b, |events| notes(events).len() == 2).await;
    let held = notes(&events);
    assert!(held[0].contains(r#"default="true""#), "{}", held[0]);
    assert!(held[1].contains(r#"default="false""#), "{}", held[1]);
    assert_eq!(
        default_notes(&session, &["main", &b]).await,
        [("main".to_owned(), 0), (b.clone(), 2)],
        "main → B → main writes both notes to B and none to the root"
    );
    let c = open_line(&session, "c").await;
    soon(session.idle("main")).await.unwrap();
    switch(b.clone()).await;
    switch(c.clone()).await;
    let events = until_events(&session, &b, |events| notes(events).len() == 4).await;
    let held = notes(&events);
    assert!(held[2].contains(r#"default="true""#), "{}", held[2]);
    assert!(held[3].contains(r#"default="false""#), "{}", held[3]);
    let at_c = until_events(&session, &c, |events| notes(events).len() == 1).await;
    assert!(
        notes(&at_c)[0].contains(r#"default="true""#),
        "{}",
        notes(&at_c)[0]
    );
    assert_eq!(
        default_notes(&session, &["main", &b, &c]).await,
        [("main".to_owned(), 0), (b.clone(), 4), (c.clone(), 1)],
        "B's losing note follows its winning one, and the root gets none"
    );
    soon(session.submit("next request".into(), InputMode::Queue, &b))
        .await
        .unwrap();
    soon(session.idle(&b)).await.unwrap();
    let events = local_events(&session, &b).await;
    let latest = notes(&events).pop().expect("B holds its notes");
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data),
            _ => None,
        })
        .expect("B's next request exists");
    assert!(
        request
            .request
            .messages
            .iter()
            .any(|message| said(message).contains(&latest)),
        "B's next request carries its latest note"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.21: a move answers only once its fact is durable. While the store holds
/// the batch, the tree keeps the old default and a conflicting end waits; the
/// committed move answers, and the end is then refused because the line it
/// would end is now the default branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_answers_only_after_its_fact_is_durable() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "continue on").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    // The line's turn and its parent's roster writes settle before the store
    // is armed, so the held batch is the move's own.
    tokio::time::sleep(Duration::from_millis(50)).await;
    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    assert_eq!(
        tree.default_branch().unwrap(),
        "main",
        "the tree keeps the default it had while the batch is queued"
    );
    let mut ended = Box::pin(tree.end(
        line.clone(),
        BranchSource::User {
            operation_id: "end".into(),
        },
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut ended)
            .await
            .is_err(),
        "an end waits for the move's admission"
    );
    store.release();
    assert_eq!(soon(&mut moved).await.unwrap(), line);
    assert_eq!(tree.default_branch().unwrap(), line);
    let refused = soon(&mut ended).await;
    assert_eq!(
        refusal(&refused).0,
        "branch_cannot_end",
        "the committed move made the line the default branch: {refused:?}"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.21: a move whose transaction fails answers its error and changes
/// nothing: the tree keeps the old default, no participant is told, and a
/// restart reads the old default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_whose_write_fails_changes_nothing() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let session_id = session.id().to_owned();
    let line = open_line(&session, "stay on main").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    // The line's turn and its parent's roster writes settle before the store
    // is armed, so the held batch is the move's own.
    tokio::time::sleep(Duration::from_millis(50)).await;
    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its batch failed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    store.release_failed();
    let failed = soon(&mut moved).await;
    assert!(failed.is_err(), "the failed write reaches the caller");
    assert_eq!(
        tree.default_branch().unwrap(),
        "main",
        "the tree keeps the default it had"
    );
    assert_eq!(
        default_notes(&session, &["main", &line]).await,
        [("main".to_owned(), 0), (line.clone(), 0)],
        "a failed move writes no note"
    );
    // A store that publishes an error faults the Session; its close is where
    // the failed batch has settled.
    let _ = soon(session.close()).await;
    let _ = soon(agent.close()).await;
    let restored = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(restored.restore_session(session_id)).await.unwrap();
    assert_eq!(
        session.default_branch().unwrap(),
        "main",
        "a restart reads the default the failed move never changed"
    );
    soon(session.close()).await.unwrap();
    soon(restored.close()).await.unwrap();
}

/// §20.20, last sentence: a recorded target that is a plugin line or was
/// created through a client before the upgrade answers the root, as one that
/// has ended does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_falls_back_to_the_root_for_a_target_it_cannot_select() {
    for source in [
        BranchSource::Host {
            operation_id: "plugin-line".into(),
        },
        BranchSource::Host {
            operation_id: "pre-upgrade-client".into(),
        },
    ] {
        let store = Arc::new(MemoryTimelineStore::default());
        let probe = Arc::new(Probe::default());
        let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
            .await
            .unwrap();
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let line = soon(session.branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: "a line".into(),
                source: source.clone(),
            },
        ))
        .await
        .unwrap();
        soon(session.idle(&line)).await.unwrap();
        let root = probe.main_timeline.lock().unwrap().clone().unwrap();
        root.append(
            EventKind::BranchDefault(BranchDefaultData {
                branch: line.clone(),
                from: None,
                source: BranchSource::User {
                    operation_id: "switch".into(),
                },
            })
            .into(),
        )
        .unwrap();
        let id = session.id();
        soon(session.close()).await.unwrap();
        soon(agent.close()).await.unwrap();
        let restored = MinimalHost::start(&builder_with_store(&Arc::new(Probe::default()), store))
            .await
            .unwrap();
        let session = soon(restored.restore_session(id.into())).await.unwrap();
        assert_eq!(
            session.default_branch().unwrap(),
            "main",
            "a {source:?} line cannot hold the role"
        );
        let tree = session.session_service::<TreeCommands>().unwrap();
        let refused = tree
            .set_default_branch(
                line.clone(),
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            )
            .await;
        assert_eq!(
            match refused {
                Err(AgentError::BranchRefused { refusal, .. }) => refusal.code(),
                other => panic!("expected a refusal, got {other:?}"),
            },
            "branch_not_selectable",
            "and it cannot be chosen either"
        );
        assert_eq!(
            local_events(&session, "main")
                .await
                .iter()
                .filter(|event| matches!(event.kind, EventKind::BranchDefault(_)))
                .count(),
            1,
            "the refusal wrote no second fact"
        );
        soon(restored.close()).await.unwrap();
    }
}

/// §20.20: a recorded target that has ended answers the root, because a move
/// can never select a line that is already ending or ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_falls_back_to_the_root_for_a_target_that_has_ended() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("a line").source,
                branch: "line".into(),
                prompt: "a line".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "line".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text("work", Some(InputMode::Queue))).into(),
        )
        .unwrap();
    child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: options("end").source,
                ancestor: None,
                final_message: None,
            })
            .into(),
        )
        .unwrap();
    main.append(
        EventKind::BranchDefault(BranchDefaultData {
            branch: "line".into(),
            from: None,
            source: BranchSource::User {
                operation_id: "switch".into(),
            },
        })
        .into(),
    )
    .unwrap();
    for timeline in [&child, &main] {
        dim_agent_runtime::core::close_timeline(timeline)
            .await
            .unwrap();
    }
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
        .await
        .unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    assert_eq!(
        session.default_branch().unwrap(),
        "main",
        "an ended target answers the root"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.21: each refusal of a move writes nothing. The four cases are the line
/// already holding the role, a plugin line, an ended line and a line whose own
/// end has committed but whose close is still ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_refusal_of_a_move_writes_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let switch = |line: &str| {
        let tree = tree.clone();
        let line = line.to_owned();
        async move {
            tree.set_default_branch(
                line,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            )
            .await
        }
    };
    let code = |result: Result<String>| match result {
        Err(AgentError::BranchRefused { refusal, .. }) => refusal.code(),
        Err(AgentError::BranchEnded(_)) => "branch_ended",
        Err(AgentError::UnknownBranch(_)) => "unknown_branch",
        other => panic!("expected a refusal, got {other:?}"),
    };
    let facts = |events: &[Arc<AgentEvent>]| {
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchDefault(_)))
            .count()
    };
    assert_eq!(
        code(switch("main").await),
        "branch_already_default",
        "the root is refused while it is the default branch"
    );
    let plugin = open_host_line(&session, "main", "a plugin line").await;
    assert_eq!(
        code(switch(&plugin).await),
        "branch_not_selectable",
        "a plugin line can never hold the role"
    );
    let line = open_line(&session, "a tool line").await;
    let ended = open_line(&session, "a line to end").await;
    soon(session.end_branch(
        &ended,
        BranchSource::User {
            operation_id: "end".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(
        code(switch(&ended).await),
        "branch_ended",
        "an ended line answers that it ended"
    );
    assert_eq!(
        local_events(&session, "main")
            .await
            .iter()
            .filter(|event| { matches!(event.kind, EventKind::BranchDefault(_)) })
            .count(),
        0,
        "no refusal wrote a fact, and the root still holds the role"
    );
    assert_eq!(session.default_branch().unwrap(), "main");
    soon(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let facts_before = facts(&local_events(&session, "main").await);
    assert_eq!(facts_before, 1, "the admitted move is the only fact");
    soon(agent.close()).await.unwrap();
}

/// §20.25: after two moves no stale instruction is left. The root holds no
/// note, A holds the note saying it lost the role, and A's end is refused while
/// it is B's ancestor and admitted once the root holds the role again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_former_default_branch_keeps_no_stale_instruction_and_ends_after_a_move_back() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    soon(session.idle("main")).await.unwrap();
    let switch = |line: String| {
        let session = session.clone();
        async move {
            soon(session.set_default_branch(
                &line,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            ))
            .await
            .unwrap()
        }
    };
    switch(a.clone()).await;
    until_events(&session, &a, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    let b = open_line(&session, "b").await;
    soon(session.idle("main")).await.unwrap();
    let cut = session.sequence(&a).unwrap();
    let nested = soon(session.branch(
        a.clone(),
        cut,
        BranchOptions {
            prompt: "nested".into(),
            source: BranchSource::User {
                operation_id: "nested".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&nested)).await.unwrap();
    switch(nested.clone()).await;
    until_events(&session, &a, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="false""#))
    })
    .await;
    until_events(&session, &nested, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    assert_eq!(
        default_notes(&session, &["main", &a, &nested]).await,
        [("main".to_owned(), 0), (a.clone(), 2), (nested.clone(), 1)],
        "the root has no note and A holds the one that says it lost the role"
    );
    let tree = session.session_service::<TreeCommands>().unwrap();
    let refused = tree
        .end(
            a.clone(),
            BranchSource::User {
                operation_id: "end".into(),
            },
        )
        .await;
    assert_eq!(
        match refused {
            Err(AgentError::BranchRefused { refusal, .. }) => refusal.code(),
            other => panic!("expected a refusal, got {other:?}"),
        },
        "branch_cannot_end",
        "A is an ancestor of the default branch"
    );
    assert_eq!(
        session
            .branches()
            .iter()
            .filter(|info| info.id == a)
            .count(),
        1,
        "the refused end leaves A open"
    );
    switch("main".into()).await;
    soon(tree.end(
        a.clone(),
        BranchSource::User {
            operation_id: "end".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(
        session
            .branches()
            .iter()
            .filter(|info| info.id == a)
            .count(),
        0,
        "with the root holding the role, A may end"
    );
    let b_notes = default_notes(&session, &[&b]).await;
    assert_eq!(b_notes, [(b.clone(), 0)], "B never held the role");
    soon(agent.close()).await.unwrap();
}

/// §20.23, second half: once another line holds the role, the former tool line
/// sends its idle notice again and may end itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_former_default_branch_resumes_its_idle_notice_and_may_end_itself() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "a line").await;
    let other = open_line(&session, "another line").await;
    soon(session.idle("main")).await.unwrap();
    soon(session.set_default_branch(
        &line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    let notices = blocks_with(&local_events(&session, "main").await, "<dim-branch-idle").len();
    soon(session.set_default_branch(
        &other,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(session.default_branch().unwrap(), other);
    let ended = local_events(&session, &line)
        .await
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::TurnEnd(_)))
        .count();
    soon(session.submit("answer again".into(), InputMode::Queue, &line))
        .await
        .unwrap();
    soon(session.idle(&line)).await.unwrap();
    until_events(&session, &line, |events| {
        events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::TurnEnd(_)))
            .count()
            > ended
    })
    .await;
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-idle").len() > notices
    })
    .await;
    let result = tool_call(&session, &line, r#"{"action":"end","content":"done"}"#).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    until_events(&session, "main", |events| {
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == line))
    })
    .await;
    soon(agent.close()).await.unwrap();
}

/// §20.26: a switch and an end are admitted in one order, and the refused one
/// writes nothing. The order is the order the two calls are made in: a switch
/// makes B the default branch, which puts A above it, and an end of A ends every
/// line below it, so whichever call completes first decides the other's answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_switch_and_an_end_admit_in_one_order_and_the_refusal_writes_nothing() {
    for end_first in [true, false] {
        let probe = Arc::new(Probe::default());
        let agent = setup(&probe).await;
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let a = open_line(&session, "a").await;
        soon(session.idle("main")).await.unwrap();
        let cut = session.sequence(&a).unwrap();
        let b = soon(session.branch(
            a.clone(),
            cut,
            BranchOptions {
                prompt: "b".into(),
                source: BranchSource::User {
                    operation_id: "b".into(),
                },
            },
        ))
        .await
        .unwrap();
        soon(session.idle(&b)).await.unwrap();
        let switch = session.set_default_branch(
            &b,
            BranchSource::User {
                operation_id: "switch".into(),
            },
        );
        let end = session.end_branch(
            &a,
            BranchSource::User {
                operation_id: "end".into(),
            },
        );
        let (switch_refused, end_refused) = if end_first {
            let ended = soon(end).await;
            assert!(ended.is_ok(), "the end is admitted first: {ended:?}");
            let switched = soon(switch).await;
            (switched.is_err(), false)
        } else {
            let switched = soon(switch).await;
            assert!(
                switched.is_ok(),
                "the switch is admitted first: {switched:?}"
            );
            let ended = soon(end).await;
            (false, ended.is_err())
        };
        assert!(
            switch_refused ^ end_refused,
            "exactly one of the two is refused (end first: {end_first})"
        );
        let facts = local_events(&session, "main")
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchDefault(_)))
            .count();
        assert_eq!(
            facts,
            usize::from(!end_first),
            "only an admitted switch writes a fact"
        );
        let ends = local_events(&session, &a)
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .count();
        assert_eq!(
            ends,
            usize::from(end_first),
            "only an admitted end writes a command"
        );
        soon(agent.close()).await.unwrap();
    }
}

/// §20.28, the tree half: a line whose stored source is `host`, as a client's
/// pre-upgrade `session.branch_create` row wrote it, is refused as a target with
/// `branch_not_selectable`, and a line opened with `User` is accepted. The
/// storage half is `a_format_52_store_is_refused_by_this_daemon_and_migrated_in_place`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_upgrade_host_line_is_refused_and_a_user_line_is_accepted() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let legacy = soon(session.branch("main".into(), 0, options("opened before the upgrade")))
        .await
        .unwrap();
    soon(session.idle(&legacy)).await.unwrap();
    let line = soon(session.branch(
        "main".into(),
        0,
        BranchOptions {
            prompt: "opened by the user".into(),
            source: BranchSource::User {
                operation_id: "client".into(),
            },
        },
    ))
    .await
    .unwrap();
    soon(session.idle(&line)).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let refused = tree
        .set_default_branch(
            legacy.clone(),
            BranchSource::User {
                operation_id: "switch".into(),
            },
        )
        .await;
    assert_eq!(
        match refused {
            Err(AgentError::BranchRefused { refusal, .. }) => refusal.code(),
            other => panic!("expected a refusal, got {other:?}"),
        },
        "branch_not_selectable",
        "a pre-upgrade host line is not a conversation line"
    );
    assert_eq!(session.default_branch().unwrap(), "main");
    soon(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(
        session.default_branch().unwrap(),
        line,
        "a line the user opened after the upgrade is accepted"
    );
    assert_eq!(
        local_events(&session, "main")
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchDefault(_)))
            .count(),
        1,
        "the refusal left no fact behind"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.26a: a switch and a tool line's own end are admitted in one order, and a
/// refused end writes no `branch/end` and no final message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_switch_and_a_tool_lines_end_admit_in_one_order_without_a_refused_ones_message() {
    for end_first in [true, false] {
        let probe = Arc::new(Probe::default());
        let agent = setup(&probe).await;
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let line = open_line(&session, "the line that ends").await;
        soon(session.idle("main")).await.unwrap();
        let tree = session.session_service::<TreeCommands>().unwrap();
        let source = BranchSource::Tool {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
        };
        let switch = session.set_default_branch(
            &line,
            BranchSource::User {
                operation_id: "switch".into(),
            },
        );
        let end = tree.end_with_final(line.clone(), source, Some("the leak is in cache.rs".into()));
        let (switch_refused, end_refused) = if end_first {
            let ended = soon(end).await;
            assert!(ended.is_ok(), "the end is admitted first: {ended:?}");
            let switched = soon(switch).await;
            (switched.is_err(), false)
        } else {
            let switched = soon(switch).await;
            assert!(
                switched.is_ok(),
                "the switch is admitted first: {switched:?}"
            );
            let ended = soon(end).await;
            (false, ended.is_err())
        };
        assert!(
            switch_refused ^ end_refused,
            "exactly one of the two is admitted (end first: {end_first})"
        );
        let finals = blocks_with(
            &local_events(&session, "main").await,
            &format!("<dim-branch-message from=\"{line}\" to=\"main\" final=\"true\">"),
        );
        let commands = local_events(&session, &line)
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .count();
        if end_first {
            assert_eq!(finals.len(), 1, "an admitted end delivers its message");
            assert_eq!(commands, 1);
            assert_eq!(session.default_branch().unwrap(), "main");
        } else {
            assert!(
                finals.is_empty(),
                "a refused end publishes no message: {finals:?}"
            );
            assert_eq!(commands, 0, "a refused end writes no command");
            assert_eq!(
                session.default_branch().unwrap(),
                line,
                "the line is still open and holds the role"
            );
            assert!(
                session.branches().iter().any(|info| info.id == line),
                "the refused end leaves the line open"
            );
        }
        soon(agent.close()).await.unwrap();
    }
}

/// §20.26b: a tool end's final message survives a stop between its `branch/end`
/// and the parent's steer. Restore delivers it once, and a second restart writes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_delivers_a_tool_ends_final_message_exactly_once() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("saved", &SessionConfig::default())
        .await
        .unwrap();
    let root = || rutis::Ctx::root().unwrap();
    let main = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let create = main
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: options("watch the tests").source,
                branch: "child".into(),
                prompt: "watch the tests".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        root(),
        TimelineKey {
            session_id: "saved".into(),
            branch_id: "child".into(),
        },
        vec![("main".into(), create.sequence)],
        store.clone(),
    );
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(create.sequence),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "watch the tests",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    let end = child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: BranchSource::Tool {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                    call_id: "call".into(),
                },
                ancestor: None,
                final_message: Some("done: the leak is fixed".into()),
            })
            .into(),
        )
        .unwrap();
    for timeline in [&child, &main] {
        dim_agent_runtime::core::close_timeline(timeline)
            .await
            .unwrap();
    }
    let finals = |events: &[Arc<AgentEvent>]| {
        blocks_with(
            events,
            "<dim-branch-message from=\"child\" to=\"main\" final=\"true\">",
        )
    };
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.restore_session("saved".into())).await.unwrap();
    let events = until_events(&session, "main", |events| !finals(events).is_empty()).await;
    let delivered = finals(&events);
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(
        delivered[0].contains("done: the leak is fixed"),
        "{}",
        delivered[0]
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data)
            if data.branch == "child" && data.end_event == end.sequence)),
        "the result follows the delivery"
    );
    let expected = format!("branch-final:child:{}", end.sequence);
    let input = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
            if data.submission_id.as_deref() == Some(expected.as_str()))
        })
        .expect("the derived id is what a second delivery would look up");
    assert_eq!(
        input.sequence,
        events
            .iter()
            .find(|event| matches!(&event.kind, EventKind::InputMessage(data)
                if finals(std::slice::from_ref(event)).len() == 1 && data.blocks.iter().any(|block| matches!(block, InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-message")))))
            .expect("the final message is an input")
            .sequence,
        "one input, under the id derived from the end"
    );
    let head = session.sequence("main").unwrap();
    let id = session.id();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
    let again = MinimalHost::start(&builder_with_store(&Arc::new(Probe::default()), store))
        .await
        .unwrap();
    let session = soon(again.restore_session(id.into())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = local_events(&session, "main").await;
    assert_eq!(
        finals(&events).len(),
        1,
        "a second restart writes nothing: {:#?}",
        finals(&events)
    );
    assert!(
        events.last().expect("history").sequence >= head,
        "the restored line keeps its history"
    );
    soon(again.close()).await.unwrap();
}

/// §5: a clean restart writes nothing. After `main -> B -> C -> D` has
/// settled, B's latest note says it handed the role to C and C's says it
/// handed it to D, while the fact names D: neither B nor C took part in
/// `C -> D`. A restart compares each line's own role with the tree and finds
/// them in agreement, so B keeps the counterpart its own note named instead
/// of gaining one that names D.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_restart_writes_no_status_note() {
    let store = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "the purpose of B").await;
    let c = open_line(&session, "the purpose of C").await;
    let d = open_line(&session, "the purpose of D").await;
    soon(session.idle("main")).await.unwrap();
    let switch = |line: &str| {
        let session = session.clone();
        let line = line.to_owned();
        async move {
            soon(session.set_default_branch(
                &line,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            ))
            .await
            .unwrap()
        }
    };
    switch(&b).await;
    switch(&c).await;
    switch(&d).await;
    owed_notes(&session, &b, 2).await;
    owed_notes(&session, &c, 2).await;
    owed_notes(&session, &d, 1).await;
    let before = default_notes(&session, &["main", &b, &c, &d]).await;
    let latest = status_notes(&local_events(&session, &b).await)
        .pop()
        .expect("B holds its notes");
    assert!(
        latest.contains(&format!(
            "The session's default branch moved from this line to line {c}"
        )),
        "B's latest note names C before the restart: {latest}"
    );
    let id = session.id().to_owned();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
    let again = MinimalHost::start(&builder_with_store(&Arc::new(Probe::default()), store))
        .await
        .unwrap();
    let session = soon(again.restore_session(id.clone())).await.unwrap();
    assert_eq!(session.default_branch().unwrap(), d, "the fact stands");
    for line in [&b, &c, &d] {
        soon(session.submit("next request".into(), InputMode::Queue, line))
            .await
            .unwrap();
        until_events(&session, line, |events| {
            request_after_resume(events).is_some()
        })
        .await;
    }
    let latest = status_notes(&local_events(&session, &b).await)
        .pop()
        .expect("B holds its notes");
    assert!(
        latest.contains(&format!(
            "The session's default branch moved from this line to line {c}"
        )),
        "B's latest note still names C: {latest}"
    );
    assert_eq!(
        default_notes(&session, &["main", &b, &c, &d]).await,
        before,
        "a clean restart adds no status note"
    );
    soon(again.close()).await.unwrap();
}

/// The `<dim-branch-line>` note origin/main (fc46080) wrote when a tool line
/// became the default branch, rendered by its own `status_note` and
/// `tool_line_body`: the birth bullet forbids opening lines, and the change
/// names no right to open one. `{line}`, `{parent}` and `{prompt}` stand where
/// that code substituted them.
const OLD_TOOL_NOTE: &str = r#"<dim-branch-line branch="{line}" parent="{parent}" source="tool" default="true">
You are line {line} of this session. Line {parent} opened you at this point of its history to work in parallel with it. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line. {parent} keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to {parent}, such as its list of open lines, are {parent}'s, not yours.

Your purpose, as {parent} wrote it:
{prompt}

How you work with {parent}:
- Work on this purpose only. You cannot open lines; if more parallel work is needed, tell {parent}.
- Read {parent} with the branch tool (action read, branch "{parent}") when what {parent} did or decided since your cut changes your work; the parent block reports when {parent} has moved. Message {parent} only for what it must act on: a decision that changes its work, a blocker only the user can settle, a conflict with another line, or a change to your purpose. Use mode "steer" for what {parent} must act on now, "append" for what {parent} should see at its next step. Do not send {parent} a progress report it can read, and do not page through {parent}'s whole stream: read the page that answers your question, then act.
- You share the workspace with {parent} and its other lines. Change only the files {parent} gave you; before changing any other file, tell {parent} which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell {parent} before you continue.
- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.
- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. If you already told {parent} that in a message, your end content carries only what is new. Read {parent} once more before you end if the parent block says {parent} has moved, so your last word still holds. That is your last action; {parent} receives it.

This line is now the session's default branch, switched from line {parent}. While this block is your latest one: answer the user here; you send no automatic idle notice to {parent}; you cannot call end on yourself. Line {parent} stays open and keeps the lines it opened.
</dim-branch-line>"#;

/// The same for a line the user opened.
const OLD_USER_NOTE: &str = r#"<dim-branch-line branch="{line}" parent="{parent}" source="user" default="true">
You are line {line} of this session, opened by the user from line {parent} at this point of its history. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line, in parallel with {parent}. You can read {parent}'s work after this point. Unless the user makes this line the default branch, you cannot call end on yourself. Messages above that were addressed to {parent} are {parent}'s, not yours. The message after this one is from the user for this line.

The user has now made this line the session's default branch, switched from line {parent}. While this block is your latest one: answer the user here; you cannot call end on yourself; the user ends this line. Line {parent} stays open and keeps the lines it opened.
</dim-branch-line>"#;

/// §5: a Session that moved its default branch under the older rule keeps that
/// note on the line, and the tool admits its `create` while the note still
/// forbids it. The load replaces it once, before the line's first request, and a
/// second load writes nothing.
async fn a_stale_note_is_replaced_once(old: &str, user: bool) {
    let purpose = "the purpose of B";
    let first = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        first.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = if user {
        let cut = session.sequence("main").unwrap();
        soon(session.branch(
            "main".into(),
            cut,
            BranchOptions {
                prompt: purpose.into(),
                source: BranchSource::User {
                    operation_id: "client".into(),
                },
            },
        ))
        .await
        .unwrap()
    } else {
        open_line(&session, purpose).await
    };
    quiet(&session, &b).await;
    switch(&session, &b).await;
    owed_notes(&session, &b, 1).await;
    let id = session.id().to_owned();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();

    // The store now holds the Session as the older code left it: B's latest
    // note is the one that code wrote, byte for byte.
    let old = old
        .replace("{line}", &b)
        .replace("{parent}", "main")
        .replace("{prompt}", purpose);
    let second = Arc::new(MemoryTimelineStore::default());
    copy_session(&first, &second, &id, |branch, mut records| {
        if branch == b {
            for record in &mut records {
                let event = Arc::make_mut(&mut record.event);
                let EventKind::InputMessage(data) = &mut event.kind else {
                    continue;
                };
                for block in &mut data.blocks {
                    if let InputBlock::Raw { text, key, retire } = block
                        && key.as_deref() == Some("branch:line")
                        && !*retire
                        && text.contains(" default=\"")
                    {
                        *text = old.clone();
                    }
                }
            }
        }
        records
    })
    .await;

    // The writer is held, so only the load can write the note it owes, and the
    // line's first request still carries it.
    let mut held = dim_agent_capabilities::timeline_tree::testing::hold_writers(&id);
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        second.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.restore_session(id.clone())).await.unwrap();
    assert_eq!(session.default_branch().unwrap(), b, "the fact stands");
    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if held.arrived().await == b {
                break;
            }
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "line {b}'s writer never stopped at the test seam"
    );
    soon(session.submit("next request".into(), InputMode::Queue, &b))
        .await
        .unwrap();
    let events = until_events(&session, &b, |events| {
        request_after_resume(events).is_some()
    })
    .await;
    let (at, request) = request_after_resume(&events).expect("B's first request exists");
    let before: Vec<_> = events
        .iter()
        .filter(|event| event.sequence < at)
        .cloned()
        .collect();
    let notes = status_notes(&before);
    assert_eq!(
        notes.len(),
        2,
        "the load replaced the stale note exactly once: {notes:?}"
    );
    assert_eq!(
        notes[0], old,
        "the stored note is the one older rules wrote"
    );
    let note = &notes[1];
    assert!(note.contains(r#"default="true""#), "{note}");
    assert!(
        note.contains("you can open lines with create"),
        "the replacement states the rule the tool applies: {note}"
    );
    assert!(
        note.contains(match user {
            true => "opened by the user from line main",
            false => "this block does not make it one: a status note would",
        }),
        "the replacement restates the current birth identity: {note}"
    );
    assert!(
        request
            .request
            .messages
            .iter()
            .any(|message| said(message).contains(note)),
        "the first request carries the note the load wrote"
    );
    held.release();
    drop(held);
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();

    let third = MinimalHost::start(&builder_with_store(&Arc::new(Probe::default()), second))
        .await
        .unwrap();
    let session = soon(third.restore_session(id)).await.unwrap();
    soon(session.idle(&b)).await.unwrap();
    assert_eq!(
        status_notes(&local_events(&session, &b).await).len(),
        2,
        "a second load finds the note it writes and adds none"
    );
    soon(third.close()).await.unwrap();
}

/// §5: the reviewer's case. A tool line was made the default branch under the
/// older rule, so its stored note forbids the `create` the tool now admits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_lines_note_from_older_rules_is_replaced_once_at_load() {
    a_stale_note_is_replaced_once(OLD_TOOL_NOTE, false).await;
}

/// §5: the same for a line the user opened and made the default branch under
/// the older rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_lines_note_from_older_rules_is_replaced_once_at_load() {
    a_stale_note_is_replaced_once(OLD_USER_NOTE, true).await;
}

/// A Session whose model compacts, over `store`, for a test that restarts it:
/// the summary strategy [`Compact`] and the hosts the branch tool needs.
fn compacting_builder(store: Arc<dyn TimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store))
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(FlowModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compact)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Compact)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}

/// A Session stored before the roster took its own key keeps the root's roster
/// under `branch:line`; its first load under the new code retires that key,
/// writes the roster under `branch:roster`, and a second load writes neither
/// (design §7.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_roots_roster_from_before_the_key_split_is_retired_at_load() {
    let first = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&compacting_builder(first.clone()))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "the purpose of the line").await;
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-lines")
            .last()
            .is_some_and(|text| text.contains(&line))
    })
    .await;
    let id = session.id().to_owned();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();

    // The stored Session as the code before the key split left it: `main`'s
    // roster stands under `branch:line`, and no line holds `branch:roster`.
    let second = Arc::new(MemoryTimelineStore::default());
    copy_session(&first, &second, &id, |branch, mut records| {
        for record in &mut records {
            let event = Arc::make_mut(&mut record.event);
            let EventKind::InputMessage(data) = &mut event.kind else {
                continue;
            };
            if branch == "main" {
                for block in &mut data.blocks {
                    if let InputBlock::Raw { key: Some(key), .. } = block
                        && key == "branch:roster"
                    {
                        *key = "branch:line".into();
                    }
                }
            } else {
                data.blocks.retain(|block| {
                    !matches!(
                        block,
                        InputBlock::Raw { key: Some(key), .. } if key == "branch:roster"
                    )
                });
            }
        }
        records
    })
    .await;

    let again = MinimalHost::start(&compacting_builder(second.clone()))
        .await
        .unwrap();
    let session = soon(again.restore_session(id.clone())).await.unwrap();
    let mut events = local_events(&session, "main").await;
    for _ in 0..500 {
        if roster_block(&events).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        events = local_events(&session, "main").await;
    }
    let (old, retired) = line_block(&events).expect("the root holds a `branch:line` block");
    assert!(
        retired,
        "the roster stored under the root's identity key is retired: {old}"
    );
    assert!(
        old.starts_with("<dim-branch-lines branch=\"main\">"),
        "{old}"
    );
    let (roster, retired) = roster_block(&events).expect("the roster is keyed `branch:roster`");
    assert!(!retired, "the roster is live: {roster}");
    assert!(
        roster.contains(&format!("- {line}: the purpose of the line")),
        "{roster}"
    );

    compact_line(&session, "main").await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let replayed = baseline_message(&events, "branch:roster").expect("the root replays its roster");
    let MessageContent::Text(text) = &replayed.content else {
        panic!("the roster is text: {replayed:?}");
    };
    assert!(
        text.contains(&format!("- {line}: the purpose of the line")),
        "{text}"
    );
    assert!(
        baseline_message(&events, "branch:line").is_none(),
        "the retired pre-upgrade roster does not replay"
    );

    let local = local_events(&session, "main").await;
    let before = (
        keyed_blocks(&local, "branch:line").len(),
        keyed_blocks(&local, "branch:roster").len(),
    );
    soon(session.close()).await.unwrap();
    soon(again.close()).await.unwrap();

    let third = MinimalHost::start(&compacting_builder(second))
        .await
        .unwrap();
    let session = soon(third.restore_session(id.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let local = local_events(&session, "main").await;
    assert_eq!(
        (
            keyed_blocks(&local, "branch:line").len(),
            keyed_blocks(&local, "branch:roster").len(),
        ),
        before,
        "a second load writes neither a roster nor a retirement"
    );
    soon(third.close()).await.unwrap();
}

/// §20.24a: a move that stopped between its fact and its notes is completed at
/// load. The first move runs live, so B holds the note production wrote; the
/// second move's fact is appended to the root by hand, with no note beside it,
/// which is what a daemon that stopped right after the commit leaves. After the
/// restart B gets its `default="false"` note and C its `default="true"` one, C's
/// names B from the fact's `from`, `main` gets none, and a second restart writes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_that_stopped_between_its_fact_and_its_notes_is_completed_at_load() {
    let store = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "the purpose of B").await;
    let c = open_line(&session, "the purpose of C").await;
    soon(session.idle("main")).await.unwrap();
    soon(session.set_default_branch(
        &b,
        BranchSource::User {
            operation_id: "switch-to-b".into(),
        },
    ))
    .await
    .unwrap();
    until_events(&session, &b, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    let id = session.id().to_owned();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();

    // The store now holds a completed first move. The second move's fact is the
    // one write the stopped daemon made.
    {
        let root_store: Arc<dyn TimelineStore> = store.clone();
        let head = root_store
            .head(&TimelineKey {
                session_id: id.clone(),
                branch_id: "main".into(),
            })
            .await
            .unwrap();
        let main = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: id.clone(),
                branch_id: "main".into(),
            },
            Vec::new(),
            root_store,
        )
        .await
        .unwrap();
        assert_eq!(main.sequence(), head, "the root reads at its stored head");
        main.append(
            EventKind::BranchDefault(BranchDefaultData {
                branch: c.clone(),
                from: Some(b.clone()),
                source: BranchSource::User {
                    operation_id: "switch-to-c".into(),
                },
            })
            .into(),
        )
        .unwrap();
        dim_agent_runtime::core::close_timeline(&main)
            .await
            .unwrap();
    }

    let mut held = dim_agent_capabilities::timeline_tree::testing::hold_writers(&id);
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.restore_session(id.clone())).await.unwrap();
    assert_eq!(session.default_branch().unwrap(), c, "the fact stands");
    let mut arrived = std::collections::BTreeSet::new();
    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while !(arrived.contains(&b) && arrived.contains(&c)) {
            arrived.insert(held.arrived().await);
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "the writers of {b} and {c} never stopped at the test seam"
    );
    for (line, role, other) in [
        (
            b.clone(),
            r#"default="false""#.to_owned(),
            format!("The session's default branch moved from this line to line {c}"),
        ),
        (
            c.clone(),
            r#"default="true""#.to_owned(),
            format!("switched from line {b}"),
        ),
    ] {
        soon(session.submit("next request".into(), InputMode::Queue, &line))
            .await
            .unwrap();
        let events = until_events(&session, &line, |events| {
            request_after_resume(events).is_some()
        })
        .await;
        let (at, request) = request_after_resume(&events).expect("the line's first request exists");
        let before: Vec<_> = events
            .iter()
            .filter(|event| event.sequence < at)
            .cloned()
            .collect();
        let note = blocks_with(&before, "<dim-branch-line ")
            .pop()
            .expect("the restored line holds its note");
        assert!(note.contains(&role), "{line}: {note}");
        assert!(note.contains(&other), "{line}: {note}");
        assert!(
            request
                .request
                .messages
                .iter()
                .any(|message| said(message).contains(&note)),
            "line {line}'s first request carries the note its load wrote"
        );
    }
    held.release();
    assert_eq!(
        default_notes(&session, &["main", &b, &c]).await,
        [("main".to_owned(), 0), (b.clone(), 2), (c.clone(), 1)],
        "B holds both notes it earned, C its one, and main none"
    );
    let heads = (session.sequence(&b).unwrap(), session.sequence(&c).unwrap());
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();
    let again = MinimalHost::start(&builder_with_store(&Arc::new(Probe::default()), store))
        .await
        .unwrap();
    let session = soon(again.restore_session(id.clone())).await.unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        default_notes(&session, &["main", &b, &c]).await,
        [("main".to_owned(), 0), (b.clone(), 2), (c.clone(), 1)],
        "a second restart writes nothing"
    );
    assert!(
        session.sequence(&b).unwrap() >= heads.0 && session.sequence(&c).unwrap() >= heads.1,
        "the restored lines keep their history"
    );
    soon(again.close()).await.unwrap();
}

/// §5, §20.24a: a restored line owes its note before it can answer a
/// request. The store holds a durable `main -> B` fact and no note, and B's
/// restored writer is stopped at the seam, so only the load can write it; the
/// first request B builds already carries it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_writes_the_owed_note_before_the_line_answers() {
    let store = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "the purpose of B").await;
    soon(session.idle("main")).await.unwrap();
    let id = session.id().to_owned();
    soon(session.close()).await.unwrap();
    soon(agent.close()).await.unwrap();

    // The store now holds the moved fact and no note beside it, which is what
    // a daemon that stopped right after the commit leaves.
    {
        let root_store: Arc<dyn TimelineStore> = store.clone();
        let key = TimelineKey {
            session_id: id.clone(),
            branch_id: "main".into(),
        };
        let head = root_store.head(&key).await.unwrap();
        let main = Timeline::restore(rutis::Ctx::root().unwrap(), key, Vec::new(), root_store)
            .await
            .unwrap();
        assert_eq!(main.sequence(), head, "the root reads at its stored head");
        main.append(
            EventKind::BranchDefault(BranchDefaultData {
                branch: b.clone(),
                from: None,
                source: BranchSource::User {
                    operation_id: "switch-to-b".into(),
                },
            })
            .into(),
        )
        .unwrap();
        dim_agent_runtime::core::close_timeline(&main)
            .await
            .unwrap();
    }

    let mut held = dim_agent_capabilities::timeline_tree::testing::hold_writers(&id);
    let agent = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.restore_session(id.clone())).await.unwrap();
    assert_eq!(session.default_branch().unwrap(), b, "the fact stands");
    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if held.arrived().await == b {
                break;
            }
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "line {b}'s writer never stopped at the test seam"
    );
    soon(session.submit("next request".into(), InputMode::Queue, &b))
        .await
        .unwrap();
    let events = until_events(&session, &b, |events| {
        request_after_resume(events).is_some()
    })
    .await;
    let (at, request) = request_after_resume(&events).expect("B's first request exists");
    let before: Vec<_> = events
        .iter()
        .filter(|event| event.sequence < at)
        .cloned()
        .collect();
    let note = blocks_with(&before, "<dim-branch-line ")
        .pop()
        .expect("B holds the note the restored fact earns");
    assert!(note.contains(r#"default="true""#), "{note}");
    assert!(note.contains("switched from line main"), "{note}");
    assert!(
        request
            .request
            .messages
            .iter()
            .any(|message| said(message).contains(&note)),
        "the first request carries the owed note before the writer runs"
    );
    held.release();
    soon(agent.close()).await.unwrap();
}

/// The first model request a line prepared after its latest `branch/resume`,
/// with its sequence: requests before the resume are the previous
/// generation's.
fn request_after_resume(events: &[Arc<AgentEvent>]) -> Option<(u64, &ContextPrepareData)> {
    let resume = events
        .iter()
        .rposition(|event| matches!(event.kind, EventKind::BranchResume(_)))?;
    events[resume..].iter().find_map(|event| match &event.kind {
        EventKind::ContextPrepare(data) => Some((event.sequence, data)),
        _ => None,
    })
}

/// The status notes a line holds, in commit order.
fn status_notes(events: &[Arc<AgentEvent>]) -> Vec<String> {
    blocks_with(events, "<dim-branch-line ")
        .into_iter()
        .filter(|text| text.contains(" default=\""))
        .collect()
}

/// Waits, bounded, for `line` to hold `count` status notes; a line left owing
/// one fails here with what it holds instead of stalling the suite.
async fn owed_notes(session: &Session, line: &str, count: usize) -> Vec<String> {
    for _ in 0..600 {
        let notes = status_notes(&local_events(session, line).await);
        if notes.len() >= count {
            return notes;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let notes = status_notes(&local_events(session, line).await);
    panic!(
        "line {line} owes {count} status notes and holds {}: a move's note is owed from the moment the line can take part",
        notes.len()
    );
}

/// Waits, bounded, for the tree to answer `line` as the default branch: an
/// admitted `branch/default` settles even when its caller went away.
async fn answers_default(tree: &TreeCommands, line: &str) {
    for _ in 0..600 {
        if tree.default_branch().is_ok_and(|default| default == line) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the tree never answered line {line} as the default branch: an admitted move must settle under the tree"
    );
}

/// Waits, bounded, for the tree to record `line` as ended: an admitted end
/// settles even when its caller went away.
async fn records_end(session: &Session, line: &str) {
    for _ in 0..600 {
        if session.ended_head(line).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("line {line} was never recorded as ended: an admitted end must settle under the tree");
}

/// §20.21: a move that commits in the window between a line becoming
/// selectable and its writer starting still owes that line its note, and a
/// queued move keeps its own role and counterpart: `main -> B -> C -> B`
/// leaves B a `true` note naming `main`, a `false` one naming C and a `true`
/// one naming C, in that order. The writer is stopped at a test seam, so the
/// three moves are committed while the lines are ready and unstarted, not by
/// racing them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_committed_before_its_writer_starts_still_owes_its_note() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let mut held = dim_agent_capabilities::timeline_tree::testing::hold_writers(session.id());
    let open = |purpose: &'static str, operation_id: &'static str| {
        let session = session.clone();
        async move {
            soon(session.branch(
                "main".into(),
                0,
                BranchOptions {
                    prompt: purpose.into(),
                    source: BranchSource::User {
                        operation_id: operation_id.into(),
                    },
                },
            ))
            .await
            .unwrap()
        }
    };
    let b = open("opened before its writer runs", "open-b").await;
    let c = open("opened before its writer runs too", "open-c").await;
    let mut arrived = std::collections::BTreeSet::new();
    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while !(arrived.contains(&b) && arrived.contains(&c)) {
            arrived.insert(held.arrived().await);
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "the writers of lines {b} and {c} never stopped at the test seam"
    );
    let tree = session.session_service::<TreeCommands>().unwrap();
    let switch = |target: &str| {
        let tree = tree.clone();
        let target = target.to_owned();
        async move {
            soon(tree.set_default_branch(
                target,
                BranchSource::User {
                    operation_id: "switch".into(),
                },
            ))
            .await
            .expect("the move commits")
        }
    };
    switch(&b).await;
    switch(&c).await;
    switch(&b).await;
    held.release();
    let notes = owed_notes(&session, &b, 3).await;
    assert!(notes[0].contains(r#"default="true""#), "{}", notes[0]);
    assert!(notes[0].contains("switched from line main"), "{}", notes[0]);
    assert!(notes[1].contains(r#"default="false""#), "{}", notes[1]);
    assert!(
        notes[1].contains(&format!(
            "The session's default branch moved from this line to line {c}"
        )),
        "{}",
        notes[1]
    );
    assert!(notes[2].contains(r#"default="true""#), "{}", notes[2]);
    assert!(
        notes[2].contains(&format!("switched from line {c}")),
        "{}",
        notes[2]
    );
    let at_c = owed_notes(&session, &c, 2).await;
    assert!(at_c[0].contains(r#"default="true""#), "{}", at_c[0]);
    assert!(
        at_c[0].contains(&format!("switched from line {b}")),
        "{}",
        at_c[0]
    );
    assert!(at_c[1].contains(r#"default="false""#), "{}", at_c[1]);
    assert!(
        at_c[1].contains(&format!(
            "The session's default branch moved from this line to line {b}"
        )),
        "{}",
        at_c[1]
    );
    assert_eq!(
        default_notes(&session, &["main", &b, &c]).await,
        [("main".to_owned(), 0), (b.clone(), 3), (c.clone(), 2)],
        "every queued move reaches its participant once, and none reaches the root"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.21: a move whose caller goes away after `branch/default` is admitted
/// settles under the tree: the fact commits, the default moves, the target is
/// protected from `end`, and its note is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_cancelled_after_admission_settles_under_the_tree() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "switched while cancelled").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    // The line's turn and its parent's roster writes settle before the store is
    // armed, so the held batch is the move's own.
    tokio::time::sleep(Duration::from_millis(50)).await;
    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    drop(moved);
    store.release();
    answers_default(&tree, &line).await;
    let refused = soon(tree.end(
        line.clone(),
        BranchSource::User {
            operation_id: "end".into(),
        },
    ))
    .await;
    assert_eq!(
        refusal(&refused).0,
        "branch_cannot_end",
        "the settled move made the line the default branch: {refused:?}"
    );
    let notes = owed_notes(&session, &line, 1).await;
    assert!(notes[0].contains(r#"default="true""#), "{}", notes[0]);
    soon(agent.close()).await.unwrap();
}

/// §20.21: a publication that fails after the caller went away still changes
/// nothing, and the tree keeps the admitted operation's guard until it has
/// settled: the next move waits for that settlement and then answers the
/// faulted root's error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_move_whose_publication_fails_keeps_the_tree_until_it_settles() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let first = open_line(&session, "first switch").await;
    let second = open_line(&session, "second switch").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        first.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    drop(moved);
    let mut next = Box::pin(tree.set_default_branch(
        second.clone(),
        BranchSource::User {
            operation_id: "next".into(),
        },
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut next)
            .await
            .is_err(),
        "a second move must wait for the admitted one to settle"
    );
    store.release_failed();
    let failed = soon(&mut next).await;
    assert!(
        failed.is_err(),
        "the settled failure reaches the tree's next operation: {failed:?}"
    );
    assert_eq!(
        tree.default_branch().unwrap(),
        "main",
        "a failed publication changes nothing"
    );
    assert_eq!(
        default_notes(&session, &["main", &first]).await,
        [("main".to_owned(), 0), (first.clone(), 0)],
        "no participant is told about a move that did not commit"
    );
    // The failed batch faults the Session; its close is where it has settled.
    let _ = soon(session.close()).await;
    let _ = soon(agent.close()).await;
}

/// §20.26b: the one end path whose settlement the caller's future owned is a
/// refused end's retried final message. Once that retry's input is admitted,
/// the tree finishes the end even if the caller goes away: the line is
/// recorded, the message is delivered once, and a later end answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_retry_cancelled_after_admission_settles_under_the_tree() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let line = open_line(&session, "deliver me").await;
    soon(session.idle("main")).await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let source = || BranchSource::Tool {
        turn_id: "turn".into(),
        request_id: "request".into(),
        call_id: "call".into(),
    };
    store.fail_next_lookup("main");
    let failed = soon(tree.end_with_final(line.clone(), source(), Some("the result".into()))).await;
    assert!(
        failed.is_err(),
        "the delivery failure reaches the caller: {failed:?}"
    );
    assert!(
        session
            .directory()
            .into_iter()
            .any(|entry| entry.info.id == line && entry.state == BranchDirectoryState::Ending),
        "the line stays ending while its message is owed"
    );
    store.hold_next_final("main");
    let mut retry =
        Box::pin(tree.end_with_final(line.clone(), source(), Some("the result".into())));
    tokio::select! {
        result = &mut retry => panic!("the retry answered before its message was admitted: {result:?}"),
        _ = store.wait_holding() => {}
    }
    drop(retry);
    store.release();
    records_end(&session, &line).await;
    let delivery = format!("branch-final:{line}:");
    let deliveries = local_events(&session, "main")
        .await
        .iter()
        .filter(|event| match &event.kind {
            EventKind::InputMessage(data) => data
                .submission_id
                .as_deref()
                .is_some_and(|id| id.starts_with(&delivery)),
            _ => false,
        })
        .count();
    assert_eq!(deliveries, 1, "the settled retry delivers the message once");
    let again = soon(tree.end(
        line.clone(),
        BranchSource::User {
            operation_id: "end".into(),
        },
    ))
    .await;
    assert!(
        matches!(again, Err(AgentError::BranchEnded(_))),
        "a later end answers once the tree recorded the end: {again:?}"
    );
    // The refused first delivery stays in the tree's cleanup errors, which
    // close reports.
    let _ = soon(agent.close()).await;
}

/// §20.21: a close that starts while an admitted move's batch is still held
/// waits for that settlement, answers within a bound once the store settles
/// the batch, and a restore reads the fact the store committed. A batch that
/// fails changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_waits_for_an_admitted_move_and_a_restore_reads_its_fact() {
    let store = Arc::new(TestStore::default());
    let agent = setup_with(&store).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id().to_owned();
    let line = move_admitted_while_held(&session, &store).await;
    let mut closing = Box::pin(session.close());
    tokio::select! {
        result = &mut closing => panic!("close answered while the move's batch was held: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    store.release();
    let closed = tokio::time::timeout(Duration::from_secs(5), &mut closing)
        .await
        .expect("close did not answer within its bound after the held batch settled");
    closed.unwrap();
    soon(agent.close()).await.unwrap();
    assert_eq!(
        restore_default(&store, &id).await,
        line,
        "a restore reads the fact the settled move committed"
    );

    let failed = Arc::new(TestStore::default());
    let agent = setup_with(&failed).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let id = session.id().to_owned();
    move_admitted_while_held(&session, &failed).await;
    let mut closing = Box::pin(session.close());
    tokio::select! {
        result = &mut closing => panic!("close answered while the move's batch was held: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    failed.release_failed();
    let closed = tokio::time::timeout(Duration::from_secs(5), &mut closing)
        .await
        .expect("close did not answer within its bound after the held batch failed");
    assert!(
        closed.is_err(),
        "the failed batch's fault reaches close: {closed:?}"
    );
    let _ = soon(agent.close()).await;
    assert_eq!(
        restore_default(&failed, &id).await,
        "main",
        "a batch that failed changes nothing"
    );
}

/// Opens a line, holds the next `branch/default` its move writes and answers
/// the line once that move's batch sits in the store, admitted and unsettled,
/// with its caller gone: the tree owns what happens to it.
async fn move_admitted_while_held(session: &Session, store: &Arc<TestStore>) -> String {
    let line = open_line(session, "moved while closing").await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        line.clone(),
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    drop(moved);
    line
}

/// Restores `id` over `store` once its Session is closed and answers the
/// default branch: what a restart reads from the facts the store holds.
async fn restore_default(store: &Arc<TestStore>, id: &str) -> String {
    let restored = MinimalHost::start(&builder_with_store(
        &Arc::new(Probe::default()),
        store.clone(),
    ))
    .await
    .unwrap();
    let session = soon(restored.restore_session(id.into())).await.unwrap();
    let default = session.default_branch().unwrap();
    soon(session.close()).await.unwrap();
    soon(restored.close()).await.unwrap();
    default
}

/// The branch tool's configuration with a Session cap, as the design's cap
/// cases state it.
fn capped(max_open_lines: usize) -> BranchToolConfig {
    BranchToolConfig {
        max_open_lines: Some(max_open_lines),
        ..BranchToolConfig::default()
    }
}

/// Moves the Session's conversation to `line` and answers once the move is
/// durable; a participant's note may still be on its way.
async fn switch(session: &Session, line: &str) {
    soon(session.set_default_branch(
        line,
        BranchSource::User {
            operation_id: "switch".into(),
        },
    ))
    .await
    .unwrap();
}

/// The head of `line` while it is idle: a committed boundary, so a turn that
/// starts afterwards cannot leave the position inside an unfinished model
/// output.
async fn idle_cut(session: &Session, line: &str) -> u64 {
    soon(session.idle(line)).await.unwrap();
    session.sequence(line).unwrap()
}

const NOT_OPENER: &str = "Only the root and the default branch open lines. Tell one of them if more parallel work is needed.";
const LIMIT_REACHED: &str = "This Session holds its limit of open lines. Continue on this line, or end one of its open lines first.";

/// The branch tool's own `create`, called as a tool call so a test can hold the
/// tree and stop the call at its admission, or let it answer there.
async fn open_with_tool(
    probe: &Probe,
    opener: &str,
    at: u64,
    prompt: &str,
) -> std::result::Result<ToolResult, HostError> {
    let registry = probe
        .tool_registry
        .lock()
        .unwrap()
        .clone()
        .expect("the Session registers its tools");
    // A read takes in what is registered now, so the tool is in the snapshot.
    registry.definitions();
    let tool = registry.contributed("branch").expect("the branch tool");
    tool.handler
        .execute(
            ToolCall {
                id: "call".into(),
                name: "branch".into(),
                input: serde_json::json!({"action": "create", "prompt": prompt, "at": at})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            },
            ToolExecutionContext {
                turn_id: "turn".into(),
                request_id: "request".into(),
                branch_id: opener.into(),
            },
            CancellationToken::new(),
        )
        .await
}

/// The refusal a tool call answers, as the tool's structured error.
fn tool_refusal(result: &ToolResult) -> (String, String) {
    let structured = result.structured_content.clone().unwrap_or_default();
    let field = |name: &str| {
        structured
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    (field("error"), field("message"))
}

/// The `branch/create` facts a line holds.
fn creates(events: &[Arc<AgentEvent>]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::BranchCreate(_)))
        .count()
}

/// §20.29: the root opens a line while another line is the default branch, so
/// the new line starts from the root's history at the cut the root names,
/// reports to the root, and the default branch hears nothing about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_root_opens_a_line_while_another_line_is_default() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    // C inherits the root's history through a completed turn: the cut ends a
    // committed batch, so no later turn can split it.
    soon(session.submit("first turn".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    let cut = idle_cut(&session, "main").await;
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let note = line_block(
        &until_events(&session, &b, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
        })
        .await,
    )
    .expect("the move wrote its note");
    assert!(
        note.0.contains("you can open lines with create"),
        "the default branch holds the right the root keeps: {}",
        note.0
    );
    let before = line_ids(&session);
    let result = tool_call(
        &session,
        "main",
        &format!(r#"{{"action":"create","prompt":"c","at":{cut}}}"#),
    )
    .await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
    let c = wait_new_line(&session, &before).await;
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == c)
        .expect("the root opened C");
    assert_eq!(
        info.parent.as_deref(),
        Some("main"),
        "C is cut from the root"
    );
    assert_eq!(
        info.branch_point,
        Some(cut),
        "C starts where the root's own history stood"
    );
    let prefix = chain_events(&session, &c, ..=cut).await.unwrap();
    assert_eq!(prefix[0].sequence, 1, "C inherits the root's history");
    let own = prefix
        .iter()
        .rposition(|event| matches!(event.kind, EventKind::BranchStart(_)))
        .expect("C's own branch/start follows the inherited prefix");
    assert_eq!(
        prefix[own - 1].sequence,
        cut,
        "C inherits the root's history through its cut and not past it"
    );
    let roster = until_events(&session, "main", |events| {
        roster_block(events).is_some_and(|(text, retired)| !retired && text.contains(&c))
    })
    .await;
    assert!(
        roster_block(&roster)
            .expect("the roster is live")
            .0
            .contains(&format!("- {c}: c")),
        "the root lists C under its purpose: {roster:?}"
    );
    let ended = tool_call(
        &session,
        &c,
        r#"{"action":"end","content":"c is finished"}"#,
    )
    .await;
    assert_eq!(ended.is_error, None, "{:?}", ended.content);
    until_events(&session, "main", |events| {
        blocks_with(events, "<dim-branch-message")
            .iter()
            .any(|text| text.contains("final=\"true\"") && text.contains("c is finished"))
    })
    .await;
    let about_c = |events: &[Arc<AgentEvent>]| {
        events.iter().any(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
                if data.blocks.iter().any(|block| matches!(block,
                    InputBlock::Raw { text, .. } if text.contains(&c) || text.contains("c is finished"))))
        })
    };
    assert!(
        !about_c(&local_events(&session, &b).await),
        "the default branch hears nothing about the root's line, neither its open nor its end"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.23: the current default branch opens a line of its own, which reports to
/// it and not to the root. The note a move writes on it states the rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_branch_opens_a_line() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let note = line_block(
        &until_events(&session, &b, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
        })
        .await,
    )
    .expect("the move wrote its note");
    assert!(
        note.0.contains("you can open lines with create"),
        "the note states the rule the tool applies: {}",
        note.0
    );
    let c = open_line_from(&session, &b, "c").await;
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == c)
        .expect("B opened C");
    assert_eq!(info.parent.as_deref(), Some(b.as_str()));
    let roster = until_events(&session, &b, |events| {
        roster_block(events).is_some_and(|(text, retired)| !retired && text.contains(&c))
    })
    .await;
    assert!(
        roster_block(&roster)
            .expect("B holds its own roster")
            .0
            .contains(&format!("- {c}: c")),
        "the opener lists the line it opened: {roster:?}"
    );
    assert!(
        !blocks_with(&local_events(&session, "main").await, "<dim-branch-lines")
            .iter()
            .any(|text| text.contains(&c)),
        "coordination follows the edge, not the Session"
    );
    let opening = opening_blocks(&local_events(&session, &c).await);
    assert!(
        opening[0].1.starts_with(&format!(
            "<dim-branch-line branch=\"{c}\" parent=\"{b}\" source=\"tool\">"
        )),
        "{:?}",
        opening[0]
    );
    soon(agent.close()).await.unwrap();
}

/// §20.32: every line but the two openers is refused `create` and writes
/// nothing. A former default branch keeps its lines, messages and reads them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_former_default_branch_keeps_its_lines_and_opens_none() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    switch(&session, &a).await;
    until_events(&session, &a, |events| {
        line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
    })
    .await;
    let t = open_line_from(&session, &a, "t").await;
    assert!(
        roster_block(&local_events(&session, &a).await)
            .is_some_and(|(text, retired)| !retired && text.contains(&t)),
        "A coordinates T while it holds the role"
    );
    switch(&session, "main").await;
    let note = line_block(
        &until_events(&session, &a, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="false""#))
        })
        .await,
    )
    .expect("the move back wrote A's note");
    assert!(
        note.0.contains("it can no longer open lines with create"),
        "the note says what A lost: {}",
        note.0
    );
    let before = line_ids(&session);
    let creates = |events: Vec<Arc<AgentEvent>>| {
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::BranchCreate(_)))
            .count()
    };
    let written = creates(local_events(&session, &a).await);
    assert_eq!(
        call(&session, &a, r#"{"action":"create","prompt":"grandchild"}"#).await,
        ("branch_not_opener".into(), NOT_OPENER.into())
    );
    assert_eq!(line_ids(&session), before, "the refusal created no line");
    assert_eq!(
        creates(local_events(&session, &a).await),
        written,
        "the refusal wrote nothing on A"
    );
    let sent = tool_call(
        &session,
        &a,
        &format!(r#"{{"action":"message","to":"{t}","content":"keep going"}}"#),
    )
    .await;
    assert_eq!(sent.is_error, None, "{:?}", sent.content);
    let page = page_of(
        &tool_call(
            &session,
            &a,
            &format!(r#"{{"action":"read","branch":"{t}"}}"#),
        )
        .await,
    );
    assert!(
        !page_sequences(&page).is_empty(),
        "A still reads the line it opened: {page:?}"
    );
    assert!(
        roster_block(&local_events(&session, &a).await)
            .is_some_and(|(text, retired)| !retired && text.contains(&format!("- {t}: t"))),
        "A keeps its roster after losing the role"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.32: a plugin line is never an opener, whatever it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_line_never_opens_a_line() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let host = open_host_line(&session, "main", "a plugin line").await;
    let before = line_ids(&session);
    assert_eq!(
        call(&session, &host, r#"{"action":"create","prompt":"child"}"#).await,
        ("branch_not_opener".into(), NOT_OPENER.into())
    );
    assert_eq!(line_ids(&session), before);
    assert!(
        !local_events(&session, &host)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreate(_))),
        "the refusal wrote nothing"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.3, §20.32: a tool line that is not the default branch is refused and
/// writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_line_still_cannot_open_a_line() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    let before = line_ids(&session);
    assert_eq!(
        call(&session, &a, r#"{"action":"create","prompt":"child"}"#).await,
        ("branch_not_opener".into(), NOT_OPENER.into())
    );
    assert_eq!(line_ids(&session), before);
    assert!(
        !local_events(&session, &a)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreate(_))),
        "the refusal wrote nothing"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.30: continuing on tool children does not fill the cap, because the
/// default branch and its ancestors do not count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cap_does_not_count_the_default_branch_or_its_ancestors() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let c = open_line_from(&session, &b, "c").await;
    switch(&session, &c).await;
    let d = open_line_from(&session, &c, "d").await;
    switch(&session, &d).await;
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == d)
        .expect("the chain reached D");
    assert_eq!(info.parent.as_deref(), Some(c.as_str()));
    let e = open_line(&session, "e").await;
    let f = open_line(&session, "f").await;
    let open = line_ids(&session);
    assert!(open.contains(&b) && open.contains(&e) && open.contains(&f));
    assert_eq!(
        call(&session, "main", r#"{"action":"create","prompt":"g"}"#).await,
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "B, C and D spent no slot: E and F filled the two"
    );
    assert_eq!(session.branches().len(), 6, "the refusal closed nothing");
    soon(agent.close()).await.unwrap();
}

/// §20.31: a switch that leaves the Session over the cap refuses new opens and
/// closes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_switch_over_the_cap_closes_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    switch(&session, &a).await;
    let b = open_line_from(&session, &a, "b").await;
    switch(&session, "main").await;
    let before = line_ids(&session);
    assert!(before.contains(&a) && before.contains(&b));
    assert_eq!(
        call(&session, "main", r#"{"action":"create","prompt":"c"}"#).await,
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "A and B count again, which is the cap"
    );
    assert_eq!(line_ids(&session), before, "the switch closed no line");
    assert_eq!(
        session.branches().len(),
        3,
        "every line open before the switch is open after it"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.34: a switch that frees a slot admits the next open, and nothing is
/// closed to free it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_switch_that_frees_a_slot_admits_the_next_open() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    let x = open_line(&session, "x").await;
    assert_eq!(
        call(&session, "main", r#"{"action":"create","prompt":"y"}"#).await,
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "A and X fill the cap while the root holds the role"
    );
    switch(&session, &a).await;
    let y = open_line(&session, "y").await;
    let open = line_ids(&session);
    assert!(
        open.contains(&a) && open.contains(&x) && open.contains(&y),
        "A stopped counting, so Y fitted; no line was closed"
    );
    soon(agent.close()).await.unwrap();
}

/// §20.33 and §20.35: the cap is charged where the line is reserved, so the
/// last slot admits one open only, and a reservation holds a slot before its
/// line publishes and frees it when the creation fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cap_is_refused_under_the_tree_lock() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    let b = open_line(&session, "b").await;
    assert_eq!(
        call(&session, "main", r#"{"action":"create","prompt":"c"}"#).await,
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "the last slot was spent by B"
    );
    assert!(
        !session
            .branches()
            .iter()
            .any(|info| info.parent.as_deref() == Some(b.as_str())),
        "the refusal opened nothing"
    );
    let _ = (a, b);
    soon(agent.close()).await.unwrap();

    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    probe.delay_child.store(true, Ordering::SeqCst);
    let builds = probe.child_builds.load(Ordering::SeqCst);
    session
        .submit(
            r#"tool:branch {"action":"create","prompt":"held"}"#.into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    soon(async {
        while probe.child_builds.load(Ordering::SeqCst) == builds {
            probe.child_entered.notified().await;
        }
    })
    .await;
    soon(session.stop("main", "user stop".into()))
        .await
        .unwrap();
    assert_eq!(
        call(&session, &b, r#"{"action":"create","prompt":"later"}"#).await,
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "the held reservation is charged with A, so B finds no room"
    );
    assert!(
        !local_events(&session, &b)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreate(_))),
        "the refusal wrote nothing on B"
    );
    probe.delay_child.store(false, Ordering::SeqCst);
    probe.child_release.notify_one();
    soon(async {
        loop {
            let failed = local_events(&session, "main")
                .await
                .iter()
                .any(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)));
            if failed {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let later = open_line_from(&session, &b, "later").await;
    assert_eq!(
        session
            .branches()
            .into_iter()
            .find(|info| info.id == later)
            .expect("the freed slot admitted the open")
            .parent
            .as_deref(),
        Some(b.as_str())
    );
    let _ = a;
    soon(agent.close()).await.unwrap();
}

/// §6.1, §14: an open decides in one order with every move and end. The root's
/// `create` is called while `A → main` holds the tree lock, before its fact is
/// published: the open waits, and then counts A as off the protected path, so
/// the cap refuses it. The refusal writes nothing and adds no line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_after_a_move_waits_and_counts_the_moves_result() {
    let store = Arc::new(TestStore::default());
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store_config(&probe, store.clone(), capped(1)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let cut = idle_cut(&session, "main").await;
    let a = open_line(&session, "a").await;
    switch(&session, &a).await;
    let tree = session.session_service::<TreeCommands>().unwrap();

    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        "main".into(),
        BranchSource::User {
            operation_id: "move".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    let before = line_ids(&session);
    let written = creates(&local_events(&session, "main").await);
    let mut created = Box::pin(open_with_tool(&probe, "main", cut, "b"));
    assert!(
        futures_util::poll!(created.as_mut()).is_pending(),
        "the open waits for the move's admission"
    );
    assert_eq!(
        line_ids(&session),
        before,
        "no line is reserved while the move settles"
    );
    store.release();
    assert_eq!(soon(&mut moved).await.unwrap(), "main");
    let created = soon(&mut created)
        .await
        .expect("the open answers a refusal");
    assert_eq!(
        tool_refusal(&created),
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "A counts once the move is in force, which is the cap: {:?}",
        created.content
    );
    assert_eq!(line_ids(&session), before, "the refusal opened no line");
    assert_eq!(
        creates(&local_events(&session, "main").await),
        written,
        "the refusal wrote nothing on the root"
    );
    soon(agent.close()).await.unwrap();
}

/// §6.1, §14: the same order for the opener. `B` holds the role and opens a line
/// while `B → main` holds the tree lock: once the move settles, `B` is no longer
/// an opener, and the refusal writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_from_the_moving_line_is_refused_once_the_move_settles() {
    let store = Arc::new(TestStore::default());
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_store(&probe, store.clone()))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let tree = session.session_service::<TreeCommands>().unwrap();

    store.hold_next_default("main");
    let mut moved = Box::pin(tree.set_default_branch(
        "main".into(),
        BranchSource::User {
            operation_id: "move".into(),
        },
    ));
    tokio::select! {
        result = &mut moved => panic!("the move answered before its fact committed: {result:?}"),
        _ = store.wait_holding() => {}
    }
    let cut = session.sequence(&b).unwrap();
    let before = line_ids(&session);
    let mut created = Box::pin(open_with_tool(&probe, &b, cut, "c"));
    assert!(
        futures_util::poll!(created.as_mut()).is_pending(),
        "the open waits for the move's admission"
    );
    assert_eq!(
        line_ids(&session),
        before,
        "no line is reserved while the move settles"
    );
    store.release();
    assert_eq!(soon(&mut moved).await.unwrap(), "main");
    let created = soon(&mut created)
        .await
        .expect("the open answers a refusal");
    assert_eq!(
        tool_refusal(&created),
        ("branch_not_opener".into(), NOT_OPENER.into()),
        "B opens lines only while it holds the role: {:?}",
        created.content
    );
    assert_eq!(line_ids(&session), before, "the refusal opened no line");
    assert!(
        !local_events(&session, &b)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreate(_))),
        "the refusal wrote nothing on B"
    );
    soon(agent.close()).await.unwrap();
}

/// §6.1, §14: the guard covers the admission only. A child whose build is paused
/// after its reservation does not keep a later move from settling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_build_does_not_hold_the_tree_lock() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let cut = idle_cut(&session, "main").await;
    let a = open_line(&session, "a").await;
    probe.delay_child.store(true, Ordering::SeqCst);
    let builds = probe.child_builds.load(Ordering::SeqCst);
    let mut created = Box::pin(open_with_tool(&probe, "main", cut, "held"));
    tokio::select! {
        result = &mut created => panic!("the paused build answered: {result:?}"),
        _ = async {
            while probe.child_builds.load(Ordering::SeqCst) == builds {
                probe.child_entered.notified().await;
            }
        } => {}
    }
    switch(&session, &a).await;
    assert_eq!(
        session.default_branch().unwrap(),
        a,
        "the move settled while the child's build was paused"
    );
    probe.delay_child.store(false, Ordering::SeqCst);
    probe.child_release.notify_one();
    let failed = patient(&mut created).await;
    assert!(
        failed.is_err(),
        "the held child's build fails, as its apply does: {failed:?}"
    );
    until_events(&session, "main", |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchCreateFailed(_)))
    })
    .await;
    soon(agent.close()).await.unwrap();
}

/// §6.1, §20.31: an open admitted before a move stands. It counts after the
/// move, which can leave the Session over the cap, and the next open is what
/// refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_admitted_before_a_move_still_stands() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(1)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let a = open_line(&session, "a").await;
    switch(&session, &a).await;
    let b = open_line(&session, "b").await;
    switch(&session, "main").await;
    let open = line_ids(&session);
    assert!(
        open.contains(&a) && open.contains(&b),
        "the move closed neither line: {open:?}"
    );
    assert_eq!(
        tool_refusal(&tool_call(&session, "main", r#"{"action":"create","prompt":"c"}"#).await),
        ("branch_limit_reached".into(), LIMIT_REACHED.into()),
        "A counts again with B, which is over the cap of one"
    );
    assert_eq!(line_ids(&session), open, "the refusal closed nothing");
    soon(agent.close()).await.unwrap();
}

/// The opener's two keys stay apart: a line that is not the root keeps the
/// identity its move wrote and the roster of the lines it opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_opener_that_is_not_the_root_keeps_its_identity_and_its_roster() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let identity = line_block(
        &until_events(&session, &b, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
        })
        .await,
    )
    .expect("B's note");
    let t = open_line_from(&session, &b, "t").await;
    let events = until_events(&session, &b, |events| {
        roster_block(events).is_some_and(|(text, retired)| !retired && text.contains(&t))
    })
    .await;
    let roster = roster_block(&events).expect("the roster is under its own key");
    assert!(!roster.1, "the roster is live: {roster:?}");
    assert!(
        roster
            .0
            .starts_with(&format!("<dim-branch-lines branch=\"{b}\">")),
        "{}",
        roster.0
    );
    assert!(
        roster.0.contains(&format!("- {t}: t")),
        "the roster lists the line B opened: {}",
        roster.0
    );
    assert_eq!(
        line_block(&events).expect("B's identity"),
        identity,
        "the roster write leaves the identity's latest block alone"
    );
    let opening = opening_blocks(&local_events(&session, &t).await);
    assert!(
        opening[0].1.starts_with(&format!(
            "<dim-branch-line branch=\"{t}\" parent=\"{b}\" source=\"tool\">"
        )),
        "{:?}",
        opening[0]
    );
    soon(agent.close()).await.unwrap();
}

/// §20.3a, second half: a line the user opened that becomes the default branch
/// opens a tool line, lists it, messages and reads it, and its notices reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_opened_default_branch_coordinates_the_line_it_opens() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let cut = session.sequence("main").unwrap();
    let u = soon(session.branch(
        "main".into(),
        cut,
        BranchOptions {
            prompt: "the user's line".into(),
            source: BranchSource::User {
                operation_id: "client".into(),
            },
        },
    ))
    .await
    .unwrap();
    quiet(&session, &u).await;
    switch(&session, &u).await;
    let t = open_line_from(&session, &u, "t").await;
    let roster = until_events(&session, &u, |events| {
        roster_block(events).is_some_and(|(text, retired)| !retired && text.contains(&t))
    })
    .await;
    assert!(
        roster_block(&roster)
            .expect("U holds a roster")
            .0
            .contains(&format!("- {t}: t")),
        "{roster:?}"
    );
    let sent = tool_call(
        &session,
        &u,
        &format!(r#"{{"action":"message","to":"{t}","content":"go on"}}"#),
    )
    .await;
    assert_eq!(sent.is_error, None, "{:?}", sent.content);
    let page = page_of(
        &tool_call(
            &session,
            &u,
            &format!(r#"{{"action":"read","branch":"{t}"}}"#),
        )
        .await,
    );
    assert!(!page_sequences(&page).is_empty(), "{page:?}");
    until_events(&session, &u, |events| {
        blocks_with(events, "<dim-branch-idle")
            .iter()
            .any(|text| text.contains(&t))
    })
    .await;
    soon(agent.close()).await.unwrap();
}

/// The status note and the roster are two keys: neither block retires the
/// other, on the way into the role and on the way out (design §7.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_status_note_and_a_live_roster_do_not_retire_each_other() {
    let probe = Arc::new(Probe::default());
    let agent = MinimalHost::start(&builder_with_branch_config(&probe, capped(2)))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let b = open_line(&session, "b").await;
    switch(&session, &b).await;
    let t = open_line_from(&session, &b, "t").await;
    until_events(&session, &b, |events| {
        roster_block(events).is_some_and(|(text, retired)| !retired && text.contains(&t))
    })
    .await;
    let note = line_block(
        &until_events(&session, &b, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="true""#))
        })
        .await,
    )
    .expect("the move in wrote B's note");
    assert!(
        note.0.starts_with(&format!(
            "<dim-branch-line branch=\"{b}\" parent=\"main\" source=\"tool\" default=\"true\">"
        )),
        "{}",
        note.0
    );
    let roster = roster_block(&local_events(&session, &b).await).expect("B's roster");
    assert!(
        !roster.1 && roster.0.contains(&format!("- {t}: t")),
        "the note left the roster alone: {roster:?}"
    );
    switch(&session, "main").await;
    let note = line_block(
        &until_events(&session, &b, |events| {
            line_block(events).is_some_and(|(text, _)| text.contains(r#"default="false""#))
        })
        .await,
    )
    .expect("the move out wrote B's note");
    assert!(
        note.0.contains("it can no longer open lines with create"),
        "{}",
        note.0
    );
    let roster = roster_block(&local_events(&session, &b).await).expect("B's roster");
    assert!(
        !roster.1 && roster.0.contains(&format!("- {t}: t")),
        "the second note left the roster alone: {roster:?}"
    );
    soon(agent.close()).await.unwrap();
}

/// E3: cold restore with two openers, the root and the default branch, each
/// holding open tool lines. Run with `--release --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e3_cold_restore_with_two_openers() {
    async fn tool_line(session: &Session, opener: &str, purpose: &str) -> String {
        let cut = session.sequence(opener).unwrap();
        let turn = format!("turn-{purpose}");
        let id = soon(session.branch(
            opener.into(),
            cut,
            BranchOptions {
                prompt: purpose.into(),
                source: BranchSource::Tool {
                    turn_id: turn.clone(),
                    request_id: format!("request-{purpose}"),
                    call_id: format!("call-{purpose}"),
                },
            },
        ))
        .await
        .unwrap();
        soon(session.idle(&id)).await.unwrap();
        id
    }
    let mut elapsed = Vec::new();
    for _ in 0..5 {
        let probe = Arc::new(Probe::default());
        let agent = setup(&probe).await;
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let opener = tool_line(&session, "main", "the default branch").await;
        switch(&session, &opener).await;
        for index in 0..2 {
            tool_line(&session, &opener, &format!("default {index}")).await;
        }
        for index in 0..2 {
            tool_line(&session, "main", &format!("root {index}")).await;
        }
        let id = session.id();
        soon(session.close()).await.unwrap();
        let started = std::time::Instant::now();
        let restored = soon(agent.restore_session(id.into())).await.unwrap();
        soon(restored.idle("main")).await.unwrap();
        elapsed.push(started.elapsed());
        soon(agent.close()).await.unwrap();
    }
    elapsed.sort();
    println!(
        "E3 cold restore with two openers and four open lines: median {:.1} ms",
        elapsed[elapsed.len() / 2].as_secs_f64() * 1e3
    );
}
