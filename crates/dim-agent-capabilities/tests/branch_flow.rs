use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::HistoryRenderer,
    host::{
        CompactionInput, CompactionStrategy, CompactionStrategyHost, ContextModel,
        ContextModelHost, HostFuture, IdentityContext, ModelChunk, ModelConnection, ModelDriver,
        ModelHost, ModelStopReason, ModelStream, PrepareContextHost, ResolveContextModel,
        StrategyDiagnostics, StrategyOutcome, ToolHost,
    },
    timeline_tree::TreeCommands,
    workspace_tools::{file_reads::FileReads, files::FileTools},
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
use support::{Broadcast, Committed};

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
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
        sequences(timeline.local(..=1).await),
        sequences(timeline.history(Some(1)).await),
        sequences(timeline.context(1).await.map(|context| context.events)),
        sequences(timeline.local(..=2).await),
        sequences(timeline.local(2..=2).await),
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
                    timeline.local(..=timeline.committed()).await?;
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
fn setup(probe: &Arc<Probe>) -> Agent {
    builder(probe).build()
}
fn builder(probe: &Arc<Probe>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    mock_hosts(&mut builder);
    defaults::install(&mut builder);
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
    builder
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
    let inside = Err(format!(
        "{:?}",
        AgentError::InvalidInput("sequence 1 is inside an atomic batch".into())
    ));
    let batch = vec![
        inside.clone(),
        inside.clone(),
        inside,
        Ok(vec![1, 2]),
        Ok(vec![2]),
    ];
    for _ in 0..2 {
        let agent = builder.build();
        let session = soon(agent.restore_session("saved".into())).await.unwrap();
        soon(session.idle("main")).await.unwrap();
        let opening = probe.opening_reads.lock().unwrap().pop().unwrap();
        let main = probe.main_timeline.lock().unwrap().clone().unwrap();
        assert_eq!(opening, batch);
        assert_eq!(batch_reads(&main).await, batch);
        assert_eq!(
            session
                .branches()
                .iter()
                .map(|info| info.id.as_str())
                .collect::<Vec<_>>(),
            ["main"]
        );
        let history = session.local("main", ..).await.unwrap();
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
    let agent = builder.build();
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
    let results: Vec<BranchEndedData> = session
        .local("main", ..)
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
    let page = session
        .local_page("ended", 0, 64, end.sequence)
        .await
        .unwrap();
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
        session
            .local_page("grand", 0, 64, grand_first.sequence)
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
    assert_eq!(info.source, Some(options("grandchild").source));
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
    let agent = builder.build();
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
    let events = session
        .local_page("X", 0, 64, session.ended_head("X").unwrap())
        .await
        .unwrap();
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
    let results: Vec<BranchEndedData> = session
        .history("main", None)
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
    let agent = builder.build();
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
    let main_events = session.local("main", ..).await.unwrap();
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
        session
            .local_page("E", 0, 64, stopped)
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
    let agent = setup(&probe);
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
    let child = session
        .local("main", ..)
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
    let events = session.local("main", ..).await.unwrap();
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
    let agent = builder.build();
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
    let agent = setup(&probe);
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

    let grandchild_head = session.sequence(&grandchild).unwrap();
    let grandchild_events = session
        .local_page(&grandchild, 0, 64, grandchild_head)
        .await
        .unwrap();
    assert!(matches!(
        grandchild_events
            .iter()
            .find(|event| matches!(event.kind, EventKind::BranchEnd(_)))
            .map(|event| &event.kind),
        Some(EventKind::BranchEnd(BranchEndData { source: end, ancestor }))
            if *end == source && ancestor.as_deref() == Some(parent.as_str())
    ));
    let parent_head = session.sequence(&parent).unwrap();
    let parent_events = session
        .local_page(&parent, 0, 64, parent_head)
        .await
        .unwrap();
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
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::BranchEnded(data)
            if data.branch == parent && data.source == source && data.ancestor.is_none()))
    );
    assert_eq!(session.sequence(&parent).unwrap(), parent_head);
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ancestors_end_closes_a_descendants_pending_tool_end() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    let agent = build.build();
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
    let agent = build.build();
    let session = soon(agent.restore_session(id)).await.unwrap();
    assert_eq!(session.ended_head(&child), None);
    let commands = probe.commands.lock().unwrap().clone().unwrap();
    assert!(commands.info(child.clone()).is_err());
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_line_opens_with_its_identity_block_and_the_creator_prompt() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = soon(session.branch("main".into(), 0, options("what this line is for")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let local = session.history(&child, None).await.unwrap();
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
             You are line {child} of this session, opened from line main at this point of its history. You are the same agent as main: everything above is your own history, and you continue from here on your own line, in parallel with main. You do not see main's work after this point. Messages above that were addressed to main are main's, not yours. The message after this one says what this line is for.\n\
             </dim-branch-line>"
        )
    );
    assert_eq!(input.effective_mode(), InputMode::Queue);
    assert!(matches!(
        input.user.as_ref().map(|user| user.content.as_slice()),
        Some([ContentBlock::Text { text }]) if text == "what this line is for"
    ));
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
    let agent = builder.build();
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
    let agent = builder.build();
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("ancestor".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let high = session.sequence("main").unwrap();
    *probe.rewrite.lock().unwrap() = Rewrite::Business(
        BranchCreateAt::Sequence { sequence: 1 },
        "final prompt".into(),
    );
    let child = soon(session.branch("main".into(), high, options("candidate")))
        .await
        .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let create = create_event(&session.local("main", ..).await.unwrap());
    let info = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap();
    assert_eq!(
        (info.branch_point, info.create_anchor),
        (Some(1), Some(create.sequence))
    );
    let local = session.local(&child, ..).await.unwrap();
    assert!(
        matches!(&local[0].kind, EventKind::BranchStart(data) if data.create_event == Some(create.sequence))
    );
    assert!(
        matches!(&local[1].kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == "final prompt"
        ))
    );
    let inherited = session.history(&child, Some(0)).await.unwrap();
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0], session.local("main", ..=1).await.unwrap()[0]);
    assert_eq!(
        session
            .local(&child, ..)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::InputApplied(_)))
            .count(),
        1
    );
    assert!(
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| event.id == create.id)
    );
    let main_timeline = probe
        .commands
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get("main")
        .unwrap();
    let trace = main_timeline.trace(create.sequence).await.unwrap().unwrap();
    assert!(matches!(
        trace.original.kind,
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Create {},
            source: BranchSource::Host { .. },
            ..
        })
    ));

    *probe.rewrite.lock().unwrap() =
        Rewrite::Business(BranchCreateAt::Create {}, "tail prompt".into());
    let tail = soon(session.branch("main".into(), 0, options("ignored")))
        .await
        .unwrap();
    let tail_create = create_event(&session.local("main", ..).await.unwrap());
    let tail_info = session
        .branches()
        .into_iter()
        .find(|info| info.id == tail)
        .unwrap();
    assert_eq!(tail_info.branch_point, Some(tail_create.sequence));
    assert_eq!(tail_info.create_anchor, Some(tail_create.sequence));
    assert_eq!(
        session.history(&tail, Some(0)).await.unwrap(),
        session
            .history("main", Some(tail_create.sequence))
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
        session.history(&grandchild, Some(0)).await.unwrap(),
        session.history(&tail, Some(0)).await.unwrap()
    );
    assert_eq!(
        probe.child_builds.load(Ordering::SeqCst),
        3,
        "inherited create events must not be dispatched"
    );
    soon(agent.close()).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_create_uses_its_anchor_and_survives_an_abandoned_waiter() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
        (Some(create.sequence), Some(create.sequence))
    );
    let prefix = session.history(&child.id, Some(0)).await.unwrap();
    assert_eq!(prefix.last().unwrap().id, create.id);
    assert!(!prefix.iter().any(|event| event.id == later.id));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_releases_an_unconsumed_create_without_waiting_for_dispatch() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
    let failures: Vec<_> = session
        .local("main", ..)
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
    let agent = setup(&probe);
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
        session
            .history(&child, None)
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
    let agent = setup(&probe);
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
    let failures: Vec<_> = session
        .local("main", ..)
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
    let agent = setup(&probe);
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
    assert!(session.local(&child, ..).await.unwrap().iter().any(
        |event| matches!(&event.kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == "accepted"
        ))
    ));
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("complete model response".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = session.local("main", ..).await.unwrap();
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
    let root = support::root();
    let agent = builder(&probe).build_in(&root);
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
    let main_events = session.local("main", ..).await.unwrap();
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
        let local = session.local(&info.id, ..).await.unwrap();
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
        let local = session.local(branch, ..).await.unwrap();
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
async fn local_events(session: &Session, branch: &str) -> Vec<Arc<AgentEvent>> {
    let head = session.sequence(branch).unwrap();
    session.local_page(branch, 0, 10_000, head).await.unwrap()
}
async fn until_events(
    session: &Session,
    branch: &str,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    for _ in 0..600 {
        if let Ok(head) = session.sequence(branch)
            && let Ok(events) = session.local_page(branch, 0, 10_000, head).await
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_branch_call_writes_nothing() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
    let before = session.local(&host, ..).await.unwrap().len();
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
    assert_eq!(session.local(&host, ..).await.unwrap().len(), before);
    let main_before = session.local("main", ..).await.unwrap().len();
    assert_eq!(
        call(&session, &tool_line, r#"{"action":"read","branch":"main"}"#).await,
        (
            "branch_not_kin".into(),
            "You can message only the lines you opened and the line that opened you.".into()
        )
    );
    let notices = ["<dim-branch-idle", "<dim-branch-lines", "<dim-branch-ended"];
    for event in session
        .local("main", ..)
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
            "You can message only the lines you opened and the line that opened you.".into()
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
        !session
            .local(&host, ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::BranchEnd(_)))
    );
    assert!(
        !session
            .local("main", ..)
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
    let agent = setup(&probe);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn main_lists_its_open_tool_lines_under_one_key() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
    let history = session.history(&first, None).await.unwrap();
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
             You are line {first} of this session. Line main opened you at this point of its history to work in parallel with it. You are the same agent as main: everything above is your own history, and you continue from here on your own line. main keeps working on its line; you do not see its work after this point, and it sees yours only through what you send it. Messages above that were addressed to main, such as its list of open lines, are main's, not yours.\n\n\
             Your purpose, as main wrote it:\nfirst purpose"
        )
    );
    soon(agent.close()).await.unwrap();
}

/// #552: `<dim-branch-line>` is written by the Session part TimelineTree, so
/// its guide entry is registered there, at the Session, and a line carries it
/// even where no branch part is loaded. `<dim-branch-lines>` stays with
/// BranchLine, which writes the roster.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="main">: on main, the runtime lists the lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;

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
    session
        .history(branch, None)
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
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    soon(session.idle("main")).await.unwrap();

    // main writes the roster and sees it: its marker and its entry.
    let main = last_system(&session, "main").await;
    assert_eq!(main.matches(IDENTITY_ENTRY).count(), 1, "{main:?}");
    assert_eq!(main.matches(ROSTER_ENTRY).count(), 1, "{main:?}");
    let roster = line_block(&session.history("main", None).await.unwrap());
    assert!(roster.is_some_and(|(text, _)| text.starts_with("<dim-branch-lines")));

    // The line carries its identity marker, and the Session's writer
    // explains it there.
    let opening = line_block(&session.history(&child, None).await.unwrap())
        .expect("the line's opening input carries its identity");
    assert!(opening.0.starts_with(&format!(
        "<dim-branch-line branch=\"{child}\" parent=\"main\" source=\"tool\">"
    )));
    let line = last_system(&session, &child).await;
    assert_eq!(line.matches(IDENTITY_ENTRY).count(), 1, "{line:?}");
    soon(agent.close()).await.unwrap();
}

/// #637: the workspace rule names the stale copy in the words the result
/// carries. The tool's side is a write through the line's own read binding,
/// the guard the design relies on (§8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_names_the_stale_file_failure_the_way_the_tool_reports_it() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let child = open_line(&session, "watch the tests").await;
    let history = session.history(&child, None).await.unwrap();
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
    let agent = setup(&probe);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = session.history("main", None).await.unwrap();
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
        "- read: show the recent activity of a line you opened, also after it ended.",
        "Pass nextCursor as after to continue.",
        "- end: on a line opened with create, end your own line.",
        "Returns the line's id.",
        "It is your last action.",
        "Lines talk only with the line that opened them and the lines they opened.",
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
async fn an_idle_line_tells_main_once_when_its_turn_ends_without_a_word() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
async fn a_turn_that_steered_main_writes_no_idle_notice() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    assert!(session.local(&child, ..).await.is_err());
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
    let agent = setup(&probe);
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
    let agent = setup(&probe);
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
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_from_outside_a_tool_line_wakes_main_with_a_notice() {
    let probe = Arc::new(Probe::default());
    let agent = setup(&probe);
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
        .map(|entry| entry.message)
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
    let agent = builder.build();
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
    let message = baseline_message(&session.history("main", None).await.unwrap(), "branch:line")
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
    let message = baseline_message(&session.history(&child, None).await.unwrap(), "branch:line")
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
    let agent = builder.build();
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
    let agent = setup(&probe);
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
    let events = session.history("main", None).await.unwrap();
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
            let events = session.history("main", None).await.unwrap();
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
    let agent = gated(&probe, &gate).build();
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(8), ender).await;
    assert!(
        outcome.is_ok(),
        "the ancestor's end waited for the line's own turn"
    );
    assert!(
        session.ended_head(&child).is_some(),
        "the line is recorded as ended"
    );
    let events = local_events(&session, &child).await;
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Cancelled { .. }))));
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
    let agent = builder1.build();
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
    let agent = builder2.build();
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
