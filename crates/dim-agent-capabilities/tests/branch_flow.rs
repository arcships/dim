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
mod support;

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
}
impl ProbePlugin {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            timeline: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
            commands: names.key()?,
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
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let commands = ctx.require_at(&self.commands)?;
            let probe = self.probe.clone();
            *probe.commands.lock().unwrap() = Some(commands);
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
                    tokio::select! { _ = probe.child_release.notified() => {}, _ = ctx.cancelled() => {} }
                    return Err(rutis::Error::from(ErrorKind::PluginFailed(
                        "test.branch-probe: the held child fails its apply".into(),
                    )));
                }
                if probe.fail_child.load(Ordering::SeqCst) {
                    tokio::select! { _ = probe.writer_ready.notified() => {}, _ = ctx.cancelled() => {} }
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
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    mock_hosts(&mut builder);
    install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_gates(probe: &Arc<Probe>, gates: &Arc<GateModel>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), gates.clone());
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_model(probe: &Arc<Probe>, model: Arc<dyn ModelDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
fn builder_with_store(probe: &Arc<Probe>, store: Arc<dyn TimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store);
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
    probe_features(&mut builder, probe);
    builder
}
/// [`builder_with`](builder_with) with `tools` in place of [`MockTools`].
fn builder_with_tools(probe: &Arc<Probe>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
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
/// as new activity ([timeline branches §7.1.1](../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
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
    builder.timeline_store(store);
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
    builder.timeline_store(store);
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
            })
            .into(),
        )
        .unwrap();
    let child_end = child
        .append(
            EventKind::BranchEnd(BranchEndData {
                source: source.clone(),
                ancestor: Some("X".into()),
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
    builder.timeline_store(store);
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
/// ([§7.1.1](../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
/// Every other read, like the boundary checks a creation makes of a line,
/// keeps working, so a refusal lands on the read it was aimed at.
#[derive(Default)]
struct TestStore {
    memory: Arc<MemoryTimelineStore>,
    queries: Mutex<Vec<(String, Vec<String>)>>,
    reads: AtomicUsize,
    armed: Mutex<Option<String>>,
    refuse_ranges: bool,
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
    builder.timeline_store(store);
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
    builder.timeline_store(store.clone());
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
    assert!(matches!(
        session.end_branch("main", source.clone()).await,
        Err(AgentError::InvalidInput(_))
    ));
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
        Some(EventKind::BranchEnd(BranchEndData { source: end, ancestor }))
            if *end == source && ancestor.as_deref() == Some(parent.as_str())
    ));
    let parent_events = line_events(&session, &parent).await.unwrap();
    assert!(matches!(
        parent_events
            .iter()
            .find(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .map(|event| &event.kind),
        Some(EventKind::BranchEnd(BranchEndData { source: end, ancestor }))
            if *end == source && ancestor.is_none()
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
    build.timeline_store(store.clone());
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
    build.timeline_store(store);
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
    builder.timeline_store(store.clone());
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
    builder.timeline_store(store.clone());
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
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::InputMessage(data) => data.blocks.iter().rev().find_map(|block| match block {
            InputBlock::Raw {
                text,
                key: Some(key),
                retire,
            } if key == "branch:line" => Some((text.clone(), *retire)),
            _ => None,
        }),
        _ => None,
    })
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
            "branch_not_main".into(),
            "Only main opens lines. Tell main if more parallel work is needed.".into()
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
            "Only a line opened with create can end itself.".into()
        )
    );
    assert_eq!(
        call(&session, &host, r#"{"action":"end","content":"done"}"#).await,
        (
            "branch_cannot_end".into(),
            "Only a line opened with create can end itself.".into()
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
    let before: std::collections::BTreeSet<String> = session
        .branches()
        .iter()
        .map(|info| info.id.clone())
        .collect();
    let call = serde_json::json!({"action": "create", "prompt": purpose}).to_string();
    let result = tool_call(session, "main", &call).await;
    assert_eq!(result.is_error, None, "{:?}", result.content);
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
async fn main_lists_its_open_tool_lines_under_one_key() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe).await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let first = open_line(&session, "first purpose").await;
    let second = open_line(&session, "second purpose\nmore detail").await;
    let events = until_events(&session, "main", |events| {
        line_block(events).is_some_and(|(text, retired)| !retired && text.contains(&second))
    })
    .await;
    let (text, retired) = line_block(&events).unwrap();
    assert!(!retired);
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
        line_block(events).is_some_and(|(text, retired)| !retired && text.contains(&child))
    })
    .await;
    let (text, retired) = line_block(&events).unwrap();
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
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="main">: on main, the runtime lists the lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;
/// #987: the parent block can arrive only on a line with a parent, so main's
/// guide carries no entry for it.
const PARENT_ENTRY: &str = r#"<dim-branch-parent branch="P" head="H" after="A">: on a line, the runtime reports that the line it was cut from has moved past where this line last read it, and where the next read starts. A later block of this name replaces the earlier one; it is state about that line, not a request."#;
const MESSAGE_ENTRY: &str = r#"<dim-branch-message from="C" to="main">: a message from another line of this session, in that line's own words and not the operator's. dim-branch-idle and dim-branch-ended are the runtime's reports about a line you opened: information, not requests."#;

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
    let roster = line_block(&chain_events(&session, "main", ..).await.unwrap());
    assert!(roster.is_some_and(|(text, _)| text.starts_with("<dim-branch-lines")));

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
/// ([§7.3](../../docs/design/timeline-branches.md#73-cache-prefix)).
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
        "- create (main only): open a line to work on prompt in parallel.",
        "- message: send content to a line you opened, or to the line that opened you.",
        "Mode \"append\" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line.",
        "- read: show a page of the activity of the line you were cut from, or of a line you opened, also after it ended.",
        "With no after, a read continues after your last read of that line: from your cut for the line you were cut from, and from its first event for a line you opened.",
        "Pass after to read from another point.",
        "- end: on a line opened with create, end your own line.",
        "Returns the line's id.",
        "It is your last action.",
        "A line reads the line it was cut from; main reads the lines it opened. Messages pass only between main and the lines it opened.",
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
    assert!(blocks_with(&events, "<dim-branch-message").last().unwrap().starts_with(
        &format!("<dim-branch-message from=\"main\" to=\"{child}\">\ntests are green\n</dim-branch-message>")
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)) && event.sequence > sequence),
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
    builder.timeline_store(store.clone());
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
/// ([§20.11](../../docs/design/timeline-branches.md#20-acceptance)).
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
/// ([§20.12](../../docs/design/timeline-branches.md#20-acceptance)).
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
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
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
        soon(session.idle(&inner)).await.unwrap();
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
/// ([§7.1.1](../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_line_whose_opening_read_fails_is_a_failed_create() {
    let store = Arc::new(TestStore::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(store.clone())
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
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
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
        soon(session.idle(&child)).await.unwrap();
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
            matches!(&event.kind, EventKind::TurnEnd(data) if matches!(&data.outcome, TurnOutcome::Completed { .. }))
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
                && matches!(&event.kind, EventKind::TurnEnd(data) if matches!(&data.outcome, TurnOutcome::Completed { .. }))
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
            |event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == steer.id),
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
        !events.iter().any(
            |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == steer.id)
        ),
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
        line_block(events) == Some((retired.clone(), true))
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
    assert!(line_block(&local_events(&session, "main").await).is_none());
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
    assert!(line_block(&events).is_none());
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
        InputBlock::Raw { key: Some(key), retire: true, .. } if key == "branch:line")));
    let child_events = local_events(&session, &child).await;
    assert!(
        child_events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnd(data)
        if data.source == source && data.ancestor.is_none()))
    );
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
async fn the_roster_and_the_identity_replay_after_a_compaction() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
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
    let child = open_line(&session, "compact my line").await;
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session
            .submit(text, InputMode::Queue, "main")
            .await
            .unwrap();
        soon(session.idle("main")).await.unwrap();
    }
    let message = baseline_message(
        &chain_events(&session, "main", ..).await.unwrap(),
        "branch:line",
    )
    .expect("main compacted and the roster replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the roster is text: {message:?}");
    };
    assert!(
        text.starts_with("<dim-branch-lines branch=\"main\">"),
        "{text}"
    );
    assert!(text.contains(&format!("- {child}: compact my line")));
    for text in ["x".repeat(1500), "y".repeat(1500)] {
        session
            .submit(text, InputMode::Queue, &child)
            .await
            .unwrap();
        soon(session.idle(&child)).await.unwrap();
    }
    let message = baseline_message(
        &chain_events(&session, &child, ..).await.unwrap(),
        "branch:line",
    )
    .expect("the child compacted and its identity replayed");
    let MessageContent::Text(text) = &message.content else {
        panic!("the identity is text: {message:?}");
    };
    assert!(
        text.starts_with(&format!(
            "<dim-branch-line branch=\"{child}\" parent=\"main\" source=\"tool\">"
        )),
        "{text}"
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
    assert_eq!(result.is_error, Some(true));
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
    keep: impl Fn(&str, &[StoredEvent]) -> usize,
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
        let mut records = from.read(&key, 1..=branch.sequence).await.unwrap();
        let kept = keep(&branch.info.id, &records);
        records.truncate(kept);
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
    builder1.timeline_store(first.clone());
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
    copy_session(&first, &second, &id, |branch, records| {
        if branch != line {
            return records.len();
        }
        let at = records
            .iter()
            .rposition(|record| matches!(&record.event.kind, EventKind::ToolResult(_)))
            .expect("the steer's result");
        records[at].batch_end as usize
    })
    .await;
    let probe = Arc::new(Probe::default());
    let mut builder2 = builder(&probe);
    builder2.timeline_store(second);
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
