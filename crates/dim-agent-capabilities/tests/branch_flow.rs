use dim_agent_capabilities::{defaults, timeline_tree::TreeCommands};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::{CordisError, Ctx, Effect, TypedPlugin};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
mod support;
use support::{Broadcast, Committed};

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("branch flow completed")
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
struct ProbePlugin(Arc<Probe>, BranchScope);
impl TypedPlugin for ProbePlugin {
    type Deps = (
        InBranch<Timeline>,
        InBranch<Dispatcher>,
        InBranch<TimelineIdentity>,
        InSession<TreeCommands>,
    );
    fn name(&self) -> &str {
        "test.branch-probe"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(timeline), InBranch(dispatcher), InBranch(identity), InSession(commands)): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let probe = self.0.clone();
            *probe.commands.lock().unwrap() = Some(commands);
            if identity.branch_id == "main" {
                *probe.main_timeline.lock().unwrap() = Some(timeline.clone());
                if probe.read_while_opening.load(Ordering::SeqCst) {
                    // A rebuild read fixes S and reads all of 1..=S.
                    timeline.local(..=timeline.committed()).await?;
                    let reads = batch_reads(&timeline).await;
                    probe.opening_reads.lock().unwrap().push(reads);
                }
            } else {
                probe.child_builds.fetch_add(1, Ordering::SeqCst);
                let cleanups = probe.child_cleanups.clone();
                ctx.effect(|| {
                    Effect::Disposer(Box::new(move || {
                        cleanups.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }))
                })?;
                probe.child_entered.notify_one();
                if probe.delay_child.load(Ordering::SeqCst) {
                    tokio::select! { _ = probe.child_release.notified() => {}, _ = ctx.cancelled() => {} }
                    // Shutdown closes service admission before cancelling apply.
                    // A refused late binding is a build result, not a cleanup failure.
                    ctx.provide(Arc::new("late child binding"))?;
                }
                if probe.fail_child.load(Ordering::SeqCst) {
                    // Fail only once the cleanup writer is Active.
                    tokio::select! { _ = probe.writer_ready.notified() => {}, _ = ctx.cancelled() => {} }
                    return Err(CordisError::ServiceNotFound(
                        "/private/agent-secret/rejected-candidate".into(),
                    ));
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
impl Capability for ProbePlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.1)
    }
}
/// Child-branch capability whose cleanup writes to the branch Timeline.
struct CleanupWriter(Arc<Probe>, BranchScope);
impl TypedPlugin for CleanupWriter {
    type Deps = (InBranch<Timeline>, InBranch<TimelineIdentity>);
    fn name(&self) -> &str {
        "test.cleanup-writer"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(timeline), InBranch(identity)): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            // Only builds the flow is about to fail; other children close normally.
            if identity.branch_id == "main" || !self.0.fail_child.load(Ordering::SeqCst) {
                return Ok(Effect::Done);
            }
            let probe = self.0.clone();
            ctx.effect(move || {
                Effect::Disposer(Box::new(move || {
                    let written = timeline.append(interrupt().into()).is_ok();
                    *probe.cleanup_write.lock().unwrap() = Some(written);
                    Ok(())
                }))
            })?;
            self.0.writer_ready.notify_one();
            Ok(Effect::Done)
        })
    }
}
impl Capability for CleanupWriter {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.1)
    }
}
fn setup(probe: &Arc<Probe>) -> Agent {
    builder(probe).build()
}
fn builder(probe: &Arc<Probe>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    // Keep completed events for the cleanup assertions in this flow.
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let writer = probe.clone();
    builder.timeline_plugin(move |scope| CleanupWriter(writer.clone(), *scope));
    let probe = probe.clone();
    builder.timeline_plugin(move |scope| ProbePlugin(probe.clone(), *scope));
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
    // A batch at sequences 1 and 2: a read may start inside it but must
    // not end there.
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
    // Prompt delivered what changed while `branch/start` dispatched; the crash
    // came before the first input, so the creation still did not finish.
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
    // A capability wrote a status-only input while the child loaded; the
    // crash came before `branch/start` and the prompt.
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
    // Committed, then a crash before the child's directory existed.
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
        // While main opened its rebuild reads shared one read of its
        // history; reads from it end only on batch boundaries, as the
        // store's reads after the opening do.
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
        dyn Fn() -> rutis::BoxFuture<'static, std::result::Result<(), CordisError>> + Send + Sync,
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
    // A committed creation is a fact: the waiter stops at once, the build
    // goes on without it.
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
    // The build outlives its abandoned waiter and publishes the child itself.
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
    // A close already in progress cannot dispose TreeTimeline while its
    // dispatcher is held before the accepted branch/create.
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
    // Capabilities unload before the Timeline closes: their cleanup can
    // still commit terminal facts.
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
    // A rejected (empty) initial input fails the child build and rolls it back.
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
    // Notify may retain the first failed build's signal; wait for the second build.
    soon(async {
        while probe.child_builds.load(Ordering::SeqCst) < 2 {
            probe.child_entered.notified().await;
        }
    })
    .await;
    waiting.abort();
    let _ = waiting.await;
    // An abandoned caller does not abandon the build: it still rolls back.
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
    // Cover direct user input and a trusted hook selecting the same unusable cut.
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
async fn the_initial_child_input_can_immediately_create_a_grandchild() {
    let probe = Arc::new(Probe::default());
    let root = support::root();
    let agent = builder(&probe).build_in(&root);
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let broadcast = Broadcast::listen(&root, session.id());
    let child = soon(session.branch(
        "main".into(),
        0,
        options(r#"tool:branch {"prompt":"grandchild"}"#),
    ))
    .await
    .unwrap();
    soon(session.idle(&child)).await.unwrap();
    let branches = session.branches();
    let grandchild = branches
        .iter()
        .find(|info| info.parent.as_deref() == Some(&child))
        .expect("the child's tree commands can find their own timeline");
    soon(session.idle(&grandchild.id)).await.unwrap();
    let child_events = session.local(&child, ..).await.unwrap();
    assert!(child_events.iter().any(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.results[0].result.is_error != Some(true))));
    let model_start = child_events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ModelStart(_)))
        .unwrap();
    assert_eq!(grandchild.branch_point, Some(model_start.sequence - 1));
    let anchor = create_event(&child_events);
    assert_eq!(grandchild.create_anchor, Some(anchor.sequence));
    soon(agent.close()).await.unwrap();
    // Every dispatched event after the listener is broadcast once, in branch
    // order, as the Timeline's own event; inherited prefixes are not repeated.
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
        let local = session.local(&info.id, ..).await.unwrap();
        assert_eq!(
            last.get(&info.id),
            local.last().map(|event| &event.sequence)
        );
    }
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

type TreeDispatch = (
    Arc<dim_agent_capabilities::timeline_tree::TimelineTree>,
    Arc<Dispatcher>,
    Ctx,
);
struct CaptureTreeDispatch {
    ports: Arc<Mutex<Option<TreeDispatch>>>,
    scope: BranchScope,
}
impl TypedPlugin for CaptureTreeDispatch {
    type Deps = (
        InSession<dim_agent_capabilities::timeline_tree::TimelineTree>,
        InBranch<Dispatcher>,
        InBranch<TimelineIdentity>,
    );
    fn name(&self) -> &str {
        "test.capture-tree-dispatch"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InSession(tree), InBranch(dispatcher), InBranch(identity)): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            if identity.branch_id == "main" {
                *self.ports.lock().unwrap() = Some((tree, dispatcher, ctx.clone()));
            }
            Ok(Effect::Done)
        })
    }
}
impl Capability for CaptureTreeDispatch {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

#[tokio::test]
async fn closing_after_dispatch_before_the_build_first_poll_is_a_normal_cancellation() {
    let ports = Arc::new(Mutex::new(None));
    let captured = ports.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.timeline_plugin(move |scope| CaptureTreeDispatch {
        ports: captured.clone(),
        scope: *scope,
    });
    let agent = builder.build();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let (tree, dispatcher, root) = ports.lock().unwrap().take().unwrap();
    // Installed after readiness: this handler follows TreeTimeline's registration.
    // The single-thread runtime cannot poll the spawned build before this callback.
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
    // The probe holds the child build; once released, this build fails.
    probe.delay_child.store(true, Ordering::SeqCst);
    session
        .submit(
            r#"tool:branch {"prompt":"child"}"#.into(),
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
