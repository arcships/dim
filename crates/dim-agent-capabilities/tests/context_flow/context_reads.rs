use super::*;
use rutis::host::CtxHost;
use std::ops::RangeInclusive;

type DeferredPublication = (Vec<StoredEvent>, PublishCallback, Result<()>);

/// A memory store that counts context reads, records the range of every
/// event read and can fail, pause or defer chosen calls.
#[derive(Default)]
pub(super) struct CountContextReads {
    memory: MemoryTimelineStore,
    reads: AtomicUsize,
    attempts: AtomicUsize,
    earliest_read: std::sync::atomic::AtomicU64,
    /// Each segment of every `read` and `read_segments`, in call order.
    pub(super) ranges: std::sync::Mutex<Vec<(String, RangeInclusive<u64>)>>,
    /// The branch, cursor and number of events of every `read_page`, in
    /// call order.
    pub(super) pages: std::sync::Mutex<Vec<(String, u64, usize)>>,
    /// Fails the given occurrence of a read of this branch and range with a
    /// storage error, counted from when it is set.
    pub(super) fail_range: std::sync::Mutex<Option<ReadOccurrence>>,
    /// Holds the given occurrence of a read of this branch and range:
    /// signals `entered`, then waits for `release`.
    pub(super) hold_range: std::sync::Mutex<Option<ReadOccurrence>>,
    fail_at: AtomicUsize,
    reject_at: AtomicUsize,
    pause_at: AtomicUsize,
    fail_snapshot: bool,
    /// Pauses the first context read cut at a `step/model-start`: Loop's
    /// preparation read, whatever other reads run around it.
    pause_preparation: std::sync::atomic::AtomicBool,
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
    publication: Option<tokio::sync::mpsc::UnboundedSender<DeferredPublication>>,
}
/// A branch, a range and which read of it, counted from 1.
pub(super) type ReadOccurrence = (String, RangeInclusive<u64>, usize);

tokio::task_local! {
    /// While set, also receives each segment that a read made by the task
    /// sets it around reads from the store.
    pub(super) static TASK_READS: std::cell::RefCell<Vec<(String, RangeInclusive<u64>)>>;
}

#[derive(Clone, Default)]
struct ModelReload(Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>);
impl ModelReload {
    /// `model` is the process ModelHost; restarting the Session's context
    /// hosts reloads ContextPreparation, and so ModelProvider, and Loop.
    fn install(&self, builder: &mut AgentBuilder, model: Arc<CancelThenEcho>) {
        let views = self.0.clone();
        builder.process_value(Arc::new(ModelHost(model.clone())));
        builder.require_feature(Feature::new("test.context-hosts-owner").session(
            "test.context-hosts-owner",
            move |names| {
                Ok(Box::new(ContextHostsOwner {
                    keys: (names.key()?, names.key()?, names.key()?),
                    model: model.clone(),
                    views: views.clone(),
                }))
            },
        ));
    }

    async fn restart(&self) {
        let view = self.0.lock().unwrap().last().cloned().expect("a Session");
        view.restart().await.unwrap();
    }
}

/// Where the Session's context hosts are provided.
type HostKeys = (
    rutis::host::Key<PrepareContextHost>,
    rutis::host::Key<ContextModelHost>,
    rutis::host::Key<CompactionStrategyHost>,
);
struct ContextHostsOwner {
    keys: HostKeys,
    model: Arc<CancelThenEcho>,
    views: Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>,
}
impl Plugin for ContextHostsOwner {
    type Deps = ();
    fn name(&self) -> &str {
        "test.context-hosts-owner"
    }

    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let view =
            mount_active(ctx, TestContextHosts(self.keys.clone(), self.model.clone())).await?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}

struct TestContextHosts(HostKeys, Arc<CancelThenEcho>);
impl Plugin for TestContextHosts {
    type Deps = ();
    fn name(&self) -> &str {
        "test.context-hosts"
    }

    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let (prepare, model, strategy) = &self.0;
        ctx.provide_at(
            prepare.clone(),
            Arc::new(PrepareContextHost(self.1.clone())),
        )?;
        ctx.provide_at(model.clone(), Arc::new(ContextModelHost(None)))?;
        ctx.provide_at(strategy.clone(), Arc::new(CompactionStrategyHost(None)))?;
        Ok(())
    }
}

struct CancelThenEcho {
    preparation: bool,
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
}
impl PrepareContext for CancelThenEcho {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        Box::pin(async move {
            if self.preparation && self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                cancel.cancelled().await;
                return Err(HostError::Cancelled("old preparation cancelled".into()));
            }
            Ok(request)
        })
    }
}
impl ModelDriver for CancelThenEcho {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !self.preparation && self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Box::pin(async move {
                self.entered.notify_one();
                cancel.cancelled().await;
                Err(HostError::Cancelled("old generation cancelled".into()))
            })
        } else {
            EchoModel.invoke(request, connection, cancel)
        }
    }
}
impl CountContextReads {
    /// Records a read of `range` of `branch`, then fails or holds it when it
    /// is the occurrence set for that.
    async fn record(&self, branch: &str, range: &RangeInclusive<u64>) -> Result<()> {
        self.earliest_read
            .fetch_min(*range.start(), Ordering::SeqCst);
        self.ranges
            .lock()
            .unwrap()
            .push((branch.to_owned(), range.clone()));
        let _ = TASK_READS
            .try_with(|reads| reads.borrow_mut().push((branch.to_owned(), range.clone())));
        let due = |slot: &std::sync::Mutex<Option<ReadOccurrence>>| {
            let mut slot = slot.lock().unwrap();
            let Some((read, range_set, occurrence)) = slot.as_mut() else {
                return false;
            };
            if read != branch || range_set != range {
                return false;
            }
            *occurrence -= 1;
            let due = *occurrence == 0;
            if due {
                *slot = None;
            }
            due
        };
        if due(&self.fail_range) {
            return Err(AgentError::Storage("injected history read failure".into()));
        }
        if due(&self.hold_range) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}
impl TimelineStore for CountContextReads {
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
        if let Some(publication) = &self.publication {
            let publication = publication.clone();
            let submitted = events.clone();
            self.memory.append(
                key,
                events,
                Box::new(move |result| {
                    publication.send((submitted, publish, result)).unwrap();
                }),
            )
        } else {
            self.memory.append(key, events, publish)
        }
    }
    fn read<'a>(
        &'a self,
        key: &'a TimelineKey,
        range: RangeInclusive<u64>,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            let read = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_at.load(Ordering::SeqCst) == read {
                return Err(AgentError::Storage("injected context read failure".into()));
            }
            if self.pause_at.load(Ordering::SeqCst) == read {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if self.pause_preparation.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.record(&key.branch_id, &range).await?;
            self.memory.read(key, range).await
        })
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            let page = self.memory.read_page(key, after, limit, through).await?;
            self.pages
                .lock()
                .unwrap()
                .push((key.branch_id.clone(), after, page.len()));
            Ok(page)
        })
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        Box::pin(async move {
            for (branch, range) in &segments {
                self.record(branch, range).await?;
            }
            self.memory.read_segments(session_id, segments).await
        })
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
        if self.fail_snapshot {
            return Box::pin(async {
                Err(AgentError::Storage("injected snapshot save failure".into()))
            });
        }
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        Box::pin(async move {
            let read = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_at.load(Ordering::SeqCst) == read {
                return Err(AgentError::Storage("injected context read failure".into()));
            }
            if self.pause_at.load(Ordering::SeqCst) == read {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if self.pause_preparation.load(Ordering::SeqCst) {
                let cut = self
                    .memory
                    .read_segments(
                        &key.session_id,
                        vec![(key.branch_id.clone(), through..=through)],
                    )
                    .await?;
                let at_model_start = cut
                    .iter()
                    .flatten()
                    .any(|stored| matches!(stored.event.kind, EventKind::ModelStart(_)));
                if at_model_start && self.pause_preparation.swap(false, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
            }
            if self.reject_at.load(Ordering::SeqCst) == read {
                return Err(AgentError::InvalidInput(
                    "injected prompt adoption rejection".into(),
                ));
            }
            self.memory.latest_snapshot(key, through, interpreter).await
        })
    }
}

#[tokio::test]
async fn recovery_reads_a_fixed_committed_prefix_before_publication() {
    for fail in [false, true] {
        let (publication, mut publications) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(CountContextReads {
            publication: Some(publication),
            ..Default::default()
        });
        store
            .create_session("recovery", &SessionConfig::default())
            .await
            .unwrap();
        let timeline = Timeline::new(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: "recovery".into(),
                branch_id: "main".into(),
            },
            vec![],
            store,
        );
        let input = || EventKind::InputMessage(InputMessageData::text("pending", None)).into();
        timeline.append(input()).unwrap();
        timeline
            .append_batch_registered(vec![input(), input()], |_| Ok(()))
            .unwrap();
        let through = timeline.committed();
        timeline.append(input()).unwrap();
        let recovered = tokio::time::timeout(Duration::from_secs(1), timeline.local(..=through))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3],
            "later writes must not extend the handoff"
        );
        assert_eq!(timeline.sequence(), 0, "recovery must not need publication");
        assert!(matches!(
            timeline.local(..=2).await,
            Err(AgentError::InvalidInput(_))
        ));
        let (_, publish, result) = publications.recv().await.unwrap();
        publish(result);
        let (_, publish, result) = publications.recv().await.unwrap();
        publish(if fail {
            Err(AgentError::Storage("failed handoff".into()))
        } else {
            result
        });
        if fail {
            assert!(
                matches!(timeline.append(input()), Err(AgentError::Storage(error)) if error == "failed handoff")
            );
        } else {
            assert_eq!(timeline.sequence(), through);
            assert!(timeline.has_pending());
        }
        let (_, publish, result) = publications.recv().await.unwrap();
        publish(result);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_reload_recovers_the_old_terminal_before_its_publication() {
    for preparation in [false, true] {
        tokio::time::timeout(Duration::from_secs(5), async {
        let (publication, mut publications) = tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
        let terminal_committed = Arc::new(tokio::sync::Notify::new());
        let release_terminal = Arc::new(tokio::sync::Notify::new());
        let hold_terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let publication_task = {
            let terminal_committed = terminal_committed.clone();
            let release_terminal = release_terminal.clone();
            let hold_terminal = hold_terminal.clone();
            tokio::spawn(async move {
                while let Some((events, publish, result)) = publications.recv().await {
                    let terminal = events.iter().any(|stored| matches!(stored.event.kind, EventKind::ModelOutputEnd(_)));
                    if hold_terminal.load(Ordering::SeqCst) && terminal {
                        terminal_committed.notify_one();
                        release_terminal.notified().await;
                    }
                    publish(result);
                }
            })
        };
        let store = Arc::new(CountContextReads {
            publication: Some(publication),
            ..CountContextReads::default()
        });
        let model = Arc::new(CancelThenEcho {
            preparation,
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
        });
        let reload = ModelReload::default();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
            .session_value(Arc::new(HistoryRenderer::default()))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        reload.install(&mut builder, model.clone());
        defaults::install(&mut builder);
        builder.timeline_store(store);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        let generation = || {
            session
                .branch_diagnostics("main")
                .unwrap()
                .plugins
                .into_iter()
                .find(|plugin| plugin.name == "ModelProvider")
                .unwrap()
        };
        let before = generation().generation;
        session
            .submit("reload model".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        model.entered.notified().await;
        hold_terminal.store(true, Ordering::SeqCst);
        let restarting = reload.clone();
        let restart = tokio::spawn(async move { restarting.restart().await });
        terminal_committed.notified().await;
        loop {
            let plugin = generation();
            if plugin.generation > before && plugin.state == rutis::host::FiberState::Active {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(session.history("main", None).await.unwrap().iter().all(|event| {
            !matches!(event.kind, EventKind::ModelOutputEnd(_))
        }), "the external history must not expose the unpublished terminal");

        hold_terminal.store(false, Ordering::SeqCst);
        release_terminal.notify_one();
        restart.await.unwrap();
        loop {
            match session.idle("main").await {
                Err(AgentError::Reloading(_)) => tokio::task::yield_now().await,
                result => { result.unwrap(); break; }
            }
        }
        let first_turn = session.history("main", None).await.unwrap();
        assert_eq!(
            first_turn
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelOutputEnd(_)))
                .count(),
            1,
            "recovery must not append a second terminal for the old request"
        );
        assert!(first_turn.iter().any(|event| matches!(
            &event.kind,
            EventKind::ModelOutputEnd(data) if matches!(&data.outcome,
                ModelOutputOutcome::Cancelled { reason } if reason == CAPABILITY_RELOADING)
        )), "the old generation must normalize its cancellation before handoff");
        assert!(first_turn.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(data) if data.outcome == TurnOutcome::Failed { error: CAPABILITY_RELOADING.into() }
        )));

        let through = first_turn.last().unwrap().sequence;
        session
            .submit("after reload".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|event| {
            event.sequence > through
                && matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after reload"))
        }));
        agent.close().await.unwrap();
        publication_task.abort();
        assert!(publication_task.await.unwrap_err().is_cancelled());
    })
    .await
    .expect("model reload recovery flow must not hang");
    }
}

#[tokio::test]
async fn stop_waits_for_an_unpublished_turn_end_seen_by_a_reloaded_loop() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (publication, mut publications) = tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
        let (ended, mut ends) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let publication_task = tokio::spawn(async move {
            let mut held = false;
            while let Some((events, publish, result)) = publications.recv().await {
                if !held && let Some(end) = events.iter().find(|stored| matches!(stored.event.kind, EventKind::TurnEnd(_))) {
                    held = true;
                    ended.send(end.event.sequence).unwrap();
                    gate.notified().await;
                }
                publish(result);
            }
        });
        let model = Arc::new(CancelThenEcho {
            preparation: false,
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
        });
        let reload = ModelReload::default();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
            .session_value(Arc::new(HistoryRenderer::default()))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            )
            .timeline_store(Arc::new(CountContextReads {
                publication: Some(publication),
                ..Default::default()
            }));
        reload.install(&mut builder, model.clone());
        defaults::install(&mut builder);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        let old_loop = session.service::<dyn LoopState>("main").unwrap();
        let generation = || session.branch_diagnostics("main").unwrap().plugins
            .into_iter().find(|plugin| plugin.name == "Loop").unwrap();
        let before = generation().generation;
        session.submit("stop during reload".into(), InputMode::Queue, "main").await.unwrap();
        model.entered.notified().await;
        let stopping = session.clone();
        let mut stop = tokio::spawn(async move { stopping.stop("main", "user stop".into()).await });
        let end = ends.recv().await.unwrap();
        assert_eq!(old_loop.state().phase, LoopPhase::Stopping);
        assert!(!session.history("main", None).await.unwrap().iter()
            .any(|event| matches!(event.kind, EventKind::TurnEnd(_))));
        reload.restart().await;
        loop {
            let plugin = generation();
            if plugin.generation > before && plugin.state == rutis::host::FiberState::Active { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!(session.service::<dyn LoopState>("main").unwrap().state().phase, LoopPhase::Idle);
        drop(old_loop);
        let premature = tokio::time::timeout(Duration::from_millis(25), &mut stop).await;
        release.notify_one();
        assert!(premature.is_err(), "stop returned before turn/end {end} publication: {premature:?}");
        let returned = stop.await.unwrap().unwrap();
        assert!(returned >= end, "stop returned {returned} before turn/end {end}");
        assert_eq!(session.history("main", None).await.unwrap().iter()
            .filter(|event| matches!(event.kind, EventKind::TurnEnd(_))).count(), 1);
        session.submit("after stop".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        assert!(session.history("main", None).await.unwrap().iter()
            .any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after stop"))));
        agent.close().await.unwrap();
        publication_task.abort();
        assert!(publication_task.await.unwrap_err().is_cancelled());
    }).await.expect("stop and Loop reload must settle");
}

#[tokio::test]
async fn either_context_read_failure_ends_the_turn_and_allows_the_next_input() {
    for read in [1] {
        let store = Arc::new(CountContextReads::default());
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        store.fail_at.store(
            store.attempts.load(Ordering::SeqCst) + read,
            Ordering::SeqCst,
        );
        session
            .submit("failed input".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let failed = session.history("main", None).await.unwrap();
        assert!(
            !failed
                .iter()
                .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
        );
        assert!(failed.iter().any(
            |event| matches!(&event.kind, EventKind::ModelOutputEnd(data)
            if matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. }
                if error.contains("injected context read failure")))
        ));
        assert!(
            failed
                .iter()
                .any(|event| matches!(&event.kind, EventKind::TurnEnd(data)
            if matches!(&data.outcome, TurnOutcome::Failed { error }
                if error.contains("injected context read failure"))))
        );
        let through = failed.last().unwrap().sequence;
        session
            .submit("recovered input".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let recovered = session.history("main", None).await.unwrap();
        assert!(
            recovered
                .iter()
                .any(|event| event.sequence > through
                    && matches!(event.kind, EventKind::ModelCall(_)))
        );
        assert!(recovered.iter().any(|event| event.sequence > through
            && matches!(&event.kind, EventKind::TurnEnd(data)
                if matches!(data.outcome, TurnOutcome::Completed { .. }))));
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn each_model_step_reads_full_context_for_policy_and_streams_otherwise() {
    for (policy, fail_snapshot) in [(false, false), (true, false), (true, true)] {
        let store = Arc::new(CountContextReads {
            fail_snapshot,
            ..Default::default()
        });
        let mut builder = if policy {
            builder_with_context(
                Arc::new(EchoModel),
                HistoryRenderer::default(),
                Context {
                    threshold: Some(900.),
                    ..Context::default()
                },
            )
        } else {
            let mut builder = AgentBuilder::new(AgentConfig::default());
            defaults::mock_hosts(&mut builder);
            defaults::install(&mut builder);
            builder
        };
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        for input in ["x".repeat(1_500), "next".into(), "third".into()] {
            let before = store.reads.load(Ordering::SeqCst);
            session
                .submit(input, InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let expected = 0;
            assert_eq!(
                store.reads.load(Ordering::SeqCst) - before,
                expected,
                "policy={policy}; the no-policy path streams batches"
            );
        }
        let history = session.history("main", None).await.unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
                .count(),
            3
        );
        if policy {
            let compaction = history
                .iter()
                .rev()
                .find(|event| {
                    matches!(&event.kind,
                EventKind::ContextCompaction(data) if matches!(&data.outcome,
                    ContextCompactionResult::Completed { .. }))
                })
                .unwrap();
            let snapshot = store
                .memory
                .latest_snapshot(
                    &TimelineKey {
                        session_id: session.id().into(),
                        branch_id: "main".into(),
                    },
                    history.last().unwrap().sequence,
                    HistoryRenderer::default().checkpoint_interpreter,
                )
                .await
                .unwrap();
            if fail_snapshot {
                assert!(snapshot.is_none());
            } else {
                let snapshot = snapshot.unwrap();
                assert_eq!(snapshot.event_id, compaction.id);
                let renderer = HistoryRenderer::default();
                let checkpoint = (renderer.decode_checkpoint)(&snapshot, compaction).unwrap();
                assert!(checkpoint.resume.sequence > 1);
                let timeline = Timeline::restore(
                    Ctx::root().unwrap(),
                    TimelineKey {
                        session_id: session.id().into(),
                        branch_id: "main".into(),
                    },
                    vec![],
                    store.clone(),
                )
                .await
                .unwrap();
                store.earliest_read.store(u64::MAX, Ordering::SeqCst);
                let (recovered, _) = renderer
                    .rendered_context(&timeline, timeline.sequence())
                    .await
                    .unwrap();
                assert!(recovered.checkpoint.is_some());
                assert_eq!(
                    store.earliest_read.load(Ordering::SeqCst),
                    checkpoint.resume.sequence
                );
                dim_agent_runtime::core::close_timeline(&timeline)
                    .await
                    .unwrap();
            }
            assert_eq!(
                history
                    .iter()
                    .filter(
                        |event| matches!(&event.kind, EventKind::ContextCompaction(data)
                        if matches!(data.outcome, ContextCompactionResult::Completed { .. }))
                    )
                    .count(),
                1
            );
            if let Some(request) = history.iter().rev().find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            }) {
                assert_eq!(
                    request.messages[0].as_ref(),
                    &dim_agent_capabilities::history::summary_message("short")
                );
                assert!(
                    !request
                        .messages
                        .contains(&Arc::new(Message::text(Role::User, "x".repeat(1_500))))
                );
            }
        }
        agent.close().await.unwrap();
    }
}

/// The number of events in the context the custom checkpoint builder last
/// saw: nonzero only after the full-path snapshot builder ran.
static BUILT_EVENTS: AtomicUsize = AtomicUsize::new(0);

fn recording_projection(
    context: &ContextHistory,
    retain_from: &str,
) -> std::result::Result<ProjectionCheckpoint, String> {
    BUILT_EVENTS.store(context.events.len(), Ordering::SeqCst);
    projection_checkpoint(context, retain_from)
}

/// A replaced checkpoint builder builds the snapshot from the full context and
/// does not stop the streamed render: the two builder facts are separate.
#[tokio::test]
async fn a_custom_checkpoint_builder_takes_the_full_path() {
    BUILT_EVENTS.store(0, Ordering::SeqCst);
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer {
            checkpoint: CheckpointBuilder::Custom(recording_projection),
            ..HistoryRenderer::default()
        },
        Context {
            threshold: Some(900.),
            ..Context::default()
        },
    );
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    for input in ["x".repeat(1_500), "next".into()] {
        session
            .submit(input, InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    assert!(
        BUILT_EVENTS.load(Ordering::SeqCst) > 0,
        "the custom builder built the snapshot from the full context"
    );
    assert!(
        !store.pages.lock().unwrap().is_empty(),
        "a replaced builder does not stop the streamed render"
    );
    agent.close().await.unwrap();
}

/// A built-in checkpoint function declared as a replaced builder takes the
/// full path: it reads the branch for its snapshot instead of streaming a
/// page, and stores the snapshot the built-in builder stores.
#[tokio::test]
async fn a_builtin_function_declared_custom_takes_the_full_path() {
    let mut runs = Vec::new();
    for checkpoint in [
        CheckpointBuilder::Projection,
        CheckpointBuilder::Custom(projection_checkpoint),
    ] {
        let store = Arc::new(CountContextReads::default());
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer {
                checkpoint,
                ..HistoryRenderer::default()
            },
            Context {
                threshold: Some(900.),
                ..Context::default()
            },
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        for input in ["x".repeat(1_500), "next".into()] {
            session
                .submit(input, InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let pages = store.pages.lock().unwrap().len();
        let reads = store.ranges.lock().unwrap().len();
        let history = session.history("main", None).await.unwrap();
        let fact = history
            .iter()
            .rev()
            .find(|event| {
                matches!(&event.kind,
                    EventKind::ContextCompaction(data) if matches!(data.outcome,
                        ContextCompactionResult::Completed { .. }))
            })
            .expect("a completed compaction");
        let key = TimelineKey {
            session_id: session.id().into(),
            branch_id: "main".into(),
        };
        let renderer = HistoryRenderer::default();
        let snapshot = store
            .memory
            .latest_snapshot(
                &key,
                history.last().unwrap().sequence,
                renderer.checkpoint_interpreter,
            )
            .await
            .unwrap()
            .expect("a snapshot");
        let saved = (renderer.decode_checkpoint)(&snapshot, fact).expect("the snapshot decodes");
        runs.push((pages, reads, saved.resume.sequence));
        agent.close().await.unwrap();
    }
    let (builtin_pages, builtin_reads, builtin_resume) = runs[0];
    let (custom_pages, custom_reads, custom_resume) = runs[1];
    assert_eq!(
        custom_pages + 1,
        builtin_pages,
        "a builder declared custom streams no page for its snapshot"
    );
    assert_eq!(
        custom_reads,
        builtin_reads + 1,
        "a builder declared custom reads the branch for its snapshot"
    );
    assert_eq!(
        custom_resume, builtin_resume,
        "both builders resume at the same event"
    );
}

/// A replaced renderer renders its own entries and takes the full
/// preparation, so its compaction's snapshot is the full context's.
#[tokio::test]
async fn a_custom_render_takes_the_full_preparation_and_saves_its_snapshot() {
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer {
            render: HistoryRender::Custom(external_history),
            ..HistoryRenderer::default()
        },
        Context {
            threshold: Some(900.),
            ..Context::default()
        },
    );
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    for input in ["x".repeat(1_500), "next".into()] {
        session
            .submit(input, InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    assert!(
        store.pages.lock().unwrap().is_empty(),
        "the custom render never streams, so its snapshot is the full context's"
    );
    let history = session.history("main", None).await.unwrap();
    let request = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert!(
        request.messages.iter().any(|message| matches!(
            &message.content,
            MessageContent::Text(text) if text.starts_with("External: ")
        )),
        "the turn renders with its own renderer"
    );
    let fact = history
        .iter()
        .rev()
        .find(|event| {
            matches!(&event.kind,
                EventKind::ContextCompaction(data) if matches!(data.outcome,
                    ContextCompactionResult::Completed { .. }))
        })
        .expect("a completed compaction");
    let key = TimelineKey {
        session_id: session.id().into(),
        branch_id: "main".into(),
    };
    let default = HistoryRenderer::default();
    let snapshot = store
        .memory
        .latest_snapshot(
            &key,
            history.last().unwrap().sequence,
            default.checkpoint_interpreter,
        )
        .await
        .unwrap()
        .expect("a snapshot");
    assert!(
        (default.decode_checkpoint)(&snapshot, fact).is_some(),
        "the compaction's snapshot is the full context's and decodes"
    );
    agent.close().await.unwrap();
}

/// A compaction prepared from the streamed context saves the snapshot for its
/// own cut, so the snapshot after a second compaction still decodes and a
/// restored Timeline resumes from it.
#[tokio::test]
async fn a_second_compaction_saves_a_snapshot_that_decodes() {
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(900.),
            ..Context::default()
        },
    );
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    for input in [
        "x".repeat(1_500),
        "next".into(),
        "y".repeat(1_500),
        "last".into(),
    ] {
        session
            .submit(input, InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    let history = session.history("main", None).await.unwrap();
    let compactions: Vec<_> = history
        .iter()
        .filter(|event| {
            matches!(&event.kind,
                EventKind::ContextCompaction(data) if matches!(data.outcome,
                    ContextCompactionResult::Completed { .. }))
        })
        .collect();
    let first = *compactions.first().expect("a first compaction");
    let second = *compactions.last().expect("a newest compaction");
    assert_ne!(
        first.id, second.id,
        "the newest compaction is not the first"
    );
    let EventKind::ContextCompaction(data) = &second.kind else {
        unreachable!()
    };
    let ContextCompactionResult::Completed { retain_from, .. } = &data.outcome else {
        unreachable!()
    };
    let key = TimelineKey {
        session_id: session.id().into(),
        branch_id: "main".into(),
    };
    let renderer = HistoryRenderer::default();
    let snapshot = store
        .memory
        .latest_snapshot(
            &key,
            history.last().unwrap().sequence,
            renderer.checkpoint_interpreter,
        )
        .await
        .unwrap()
        .expect("a snapshot");
    assert_eq!(
        snapshot.event_id, second.id,
        "anchored at the last compaction"
    );
    assert!(
        (renderer.decode_checkpoint)(&snapshot, second).is_some(),
        "the snapshot decodes against the second compaction; it decodes against the first instead: {}",
        (renderer.decode_checkpoint)(&snapshot, first).is_some()
    );
    let checkpoint = (renderer.decode_checkpoint)(&snapshot, second).unwrap();
    assert_eq!(checkpoint.state.retain_from, *retain_from);
    assert!(checkpoint.resume.sequence >= 1);
    let timeline = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    store.earliest_read.store(u64::MAX, Ordering::SeqCst);
    let (recovered, restored) = renderer
        .rendered_context(&timeline, timeline.sequence())
        .await
        .unwrap();
    assert!(
        recovered.checkpoint.is_some(),
        "the restore resumes from a checkpoint"
    );
    assert_eq!(
        store.earliest_read.load(Ordering::SeqCst),
        checkpoint.resume.sequence
    );
    let full = project_context_history(&timeline.context(timeline.sequence()).await.unwrap())
        .expect("the full replay renders");
    assert_eq!(
        restored
            .iter()
            .map(|entry| entry.message.clone())
            .collect::<Vec<_>>(),
        full.iter()
            .map(|entry| entry.message.clone())
            .collect::<Vec<_>>(),
        "the restored render equals the full replay"
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_is_dispatched_while_either_context_read_is_waiting() {
    for read in [1] {
        let store = Arc::new(CountContextReads::default());
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        store.pause_at.store(
            store.attempts.load(Ordering::SeqCst) + read,
            Ordering::SeqCst,
        );
        session
            .submit("read then stop".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), store.entered.notified())
            .await
            .unwrap();
        let stopping = session.clone();
        let stop = tokio::spawn(async move {
            stopping
                .stop("main", "stop during context read".into())
                .await
        });
        let mut dispatch = session.dispatch_watch("main").unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let interrupt = session
                    .history("main", None)
                    .await
                    .unwrap()
                    .iter()
                    .find(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
                    .map(|event| event.sequence);
                if interrupt.is_some_and(|sequence| dispatch.borrow().sequence >= sequence) {
                    break;
                }
                tokio::select! {
                    _ = dispatch.changed() => {}
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
        .await
        .expect("the cold read must not block stop dispatch");
        store.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), stop)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
        );
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Cancelled { .. }))));
        agent.close().await.unwrap();
    }
}

struct SystemSources(AtomicUsize);
impl LoadPrompt for SystemSources {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let version = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(Some(PromptEntry {
                body: format!("{}:{version}", target.source),
                source: target.source,
                scope: target.scope,
                version: version.to_string(),
                delivery: PromptDelivery::System,
                order: 0,
            }))
        })
    }
}

#[tokio::test]
async fn configure_context_read_failure_keeps_the_previous_state_and_allows_retry() {
    for read in [1] {
        let store = Arc::new(CountContextReads::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(SystemSources(AtomicUsize::new(1))),
            None,
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let update = ConfigureUpdate {
            sources: vec![PromptSourceTarget {
                source: "alpha".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            }],
            model: Some(ModelSelection {
                provider_id: "provider".into(),
                model_id: "model".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
        };
        store.fail_at.store(
            store.attempts.load(Ordering::SeqCst) + read,
            Ordering::SeqCst,
        );
        assert!(matches!(session.configure(update.clone(), "main").await,
            Err(AgentError::Storage(message)) if message.contains("injected context read failure")));
        assert!(session.failure().is_none());
        assert!(
            !session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(
                    event.kind,
                    EventKind::PromptChanged(_) | EventKind::ContextConfigured(_)
                ))
        );
        assert_eq!(
            session.configure(update, "main").await.unwrap(),
            ConfigureOutcome::Adopted
        );
        session
            .submit("after configure retry".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|event|
            matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after configure retry"))));
        agent.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_system_adoptions_include_previous_unpublished_updates() {
    let (publication, mut publications) =
        tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
    tokio::spawn(async move {
        while let Some((_, publish, result)) = publications.recv().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
            publish(result);
        }
    });
    let store = Arc::new(CountContextReads {
        publication: Some(publication),
        ..CountContextReads::default()
    });
    let host = Arc::new(SystemSources(AtomicUsize::new(1)));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(&mut builder, host.clone(), None);
    builder.timeline_store(store);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                sources: Vec::new(),
                model: Some(ModelSelection {
                    provider_id: "provider".into(),
                    model_id: "model".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
            },
            "main",
        )
        .await
        .unwrap();
    for version in [1, 2] {
        host.0.store(version, Ordering::SeqCst);
        let update = |source: &str| ConfigureUpdate {
            sources: vec![PromptSourceTarget {
                source: source.into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            }],
            model: None,
        };
        let (alpha, beta) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                session.configure(update("alpha"), "main"),
                session.configure(update("beta"), "main")
            )
        })
        .await
        .expect("publication and dispatch must settle both configure calls");
        assert_eq!(alpha.unwrap(), ConfigureOutcome::Adopted);
        assert_eq!(beta.unwrap(), ConfigureOutcome::Adopted);
        let events = session.history("main", None).await.unwrap();
        let prompts: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::PromptChanged(data) => Some(data),
                _ => None,
            })
            .collect();
        for pair in prompts.windows(2) {
            assert_eq!(
                pair[1].previous.as_ref().unwrap().revision,
                pair[0].revision
            );
        }
        let configurations: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data),
                _ => None,
            })
            .collect();
        for pair in configurations.windows(2) {
            assert_eq!(
                pair[1].previous.as_ref().unwrap().revision,
                pair[0].revision
            );
        }
        let system = match &configurations.last().unwrap().system {
            Some(SystemPrompt::Text(system)) => system,
            other => panic!("system guide missing: {other:?}"),
        };
        assert!(
            system.starts_with(&format!(
                "alpha:{version}\n\nbeta:{version}\n\n# How to read your input"
            )),
            "{system:?}"
        );
        assert!(
            system.ends_with("they do not renew any permission."),
            "{system:?}"
        );
    }
    agent.close().await.unwrap();
}

#[derive(Default)]
struct InterruptGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    once: std::sync::atomic::AtomicBool,
}
struct InterruptGatePlugin {
    gate: Arc<InterruptGate>,
    dispatcher: Key<Dispatcher>,
}
impl InterruptGatePlugin {
    fn new(names: &Names<'_>, gate: Arc<InterruptGate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for InterruptGatePlugin {
    fn name(&self) -> &str {
        "test.interrupt-gate"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let gate = self.gate.clone();
            dispatcher.subscribe(ctx, "test.interrupt-gate", &["stop/interrupt"], move |_| {
                let gate = gate.clone();
                Box::pin(async move {
                    if !gate.once.swap(true, Ordering::SeqCst) {
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

/// The gate must be registered before the default capabilities;
/// ContextPreparation's service publication and Loop's subscription need not
/// complete in that order. Listener order among concurrently loading
/// capabilities is not fixed, so the gate holds Loop's interrupt only when its
/// listener comes first and the test retries Sessions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_written_after_an_interrupt_loop_has_not_handled_does_not_run() {
    let store = Arc::new(CountContextReads::default());
    let gate = Arc::new(InterruptGate::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(
            Context::default(),
        )))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(
            Context::default(),
        )))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        )
        .timeline_store(store.clone());
    let copy = gate.clone();
    builder.require_feature(
        Feature::new("test.interrupt-gate").branch("test.interrupt-gate", move |names| {
            Ok(Box::new(InterruptGatePlugin::new(names, copy.clone())?))
        }),
    );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let mut setup = None;
    for _ in 0..50 {
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        store.pause_preparation.store(true, Ordering::SeqCst);
        gate.once.store(false, Ordering::SeqCst);
        session
            .submit(
                "interrupted before preparation".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), store.entered.notified())
            .await
            .unwrap();
        let stopping = session.clone();
        let stop = tokio::spawn(async move { stopping.stop("main", "user stop".into()).await });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        if session.state("main").unwrap().stopping {
            store.release.notify_waiters();
            gate.release.notify_one();
            let _ = stop.await.unwrap();
            session.close().await.unwrap();
            continue;
        }
        setup = Some((session, stop));
        break;
    }
    let (session, stop) = setup.expect("a Session where the gate held the interrupt from Loop");
    store.release.notify_one();
    let prepare = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = session.history("main", None).await.unwrap();
            if let Some(event) = events
                .iter()
                .find(|event| matches!(event.kind, EventKind::ContextPrepare(_)))
            {
                return event.sequence;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let interrupt = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .unwrap()
        .sequence;
    assert!(prepare > interrupt, "the request follows the interrupt");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_))),
        "the late request must not reach the model"
    );
    assert!(events.iter().any(|event| matches!(
        event.kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        })
    )));
    agent.close().await.unwrap();
}

/// Streams one text block, then holds the message open until released.
struct HeldTextModel(Arc<tokio::sync::Notify>);
impl ModelDriver for HeldTextModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let release = self.0.clone();
        Box::pin(async move {
            let head = futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "held".into(),
                }),
                Ok(ModelChunk::Text("hello".into())),
                Ok(ModelChunk::TextEnd),
            ]);
            let tail = futures_util::stream::once(async move {
                release.notified().await;
                Ok(ModelChunk::MessageStop {
                    message_id: "held".into(),
                    reason: ModelStopReason::EndTurn,
                })
            });
            Ok(Box::pin(head.chain(tail)) as ModelStream)
        })
    }
}

/// Committed output stays in the progress until its event is published, so a
/// client always sees it in one of the two (ADR agent-029, decision 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_keeps_committed_output_until_it_is_published() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (publication, mut publications) =
            tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
        let held = Arc::new(tokio::sync::Notify::new());
        let release_text = Arc::new(tokio::sync::Notify::new());
        let publication_task = {
            let (held, release_text) = (held.clone(), release_text.clone());
            tokio::spawn(async move {
                while let Some((events, publish, result)) = publications.recv().await {
                    if events
                        .iter()
                        .any(|stored| matches!(stored.event.kind, EventKind::Text(_)))
                    {
                        held.notify_one();
                        release_text.notified().await;
                    }
                    publish(result);
                }
            })
        };
        let store = Arc::new(CountContextReads {
            publication: Some(publication),
            ..CountContextReads::default()
        });
        let end = Arc::new(tokio::sync::Notify::new());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(Arc::new(HeldTextModel(end.clone())))))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install(&mut builder);
        builder.timeline_store(store);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        held.notified().await;
        assert!(
            !session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(event.kind, EventKind::Text(_)))
        );
        progress
            .wait_for(|progress| progress.text == "hello")
            .await
            .unwrap();
        release_text.notify_one();
        progress
            .wait_for(|progress| progress.text.is_empty() && progress.request_id.is_some())
            .await
            .unwrap();
        assert!(
            session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == "hello"))
        );
        end.notify_one();
        session.idle("main").await.unwrap();
        progress
            .wait_for(|progress| *progress == ModelProgress::default())
            .await
            .unwrap();
        agent.close().await.unwrap();
        publication_task.abort();
    })
    .await
    .unwrap();
}

/// A steer consumed while Loop still reads the request's context replaces
/// the request before it is handed over: no `context/prepare` is written for
/// it, it ends Cancelled, and the next request of the same turn carries the
/// steer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_during_the_context_read_ends_the_request_before_it_is_handed_over() {
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder(Arc::new(EchoModel));
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    store.pause_preparation.store(true, Ordering::SeqCst);
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.entered.notified())
        .await
        .unwrap();
    session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    let events = session.history("main", None).await.unwrap();
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelStart(data) => Some(data.request_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 2);
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::ModelOutputEnd(ModelOutputEndData {
            request_id,
            outcome: ModelOutputOutcome::Cancelled { reason },
            ..
        }) if *request_id == starts[0]
            && reason == dim_agent_capabilities::steer::REPLACED_BY_STEER)));
    let prepared: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextPrepare(data) => Some(data.request_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(prepared, [starts[1].clone()]);
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.request_id == starts[1] && data.delta.contains("STEER-MARKER"))));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        1
    );
    agent.close().await.unwrap();
}
