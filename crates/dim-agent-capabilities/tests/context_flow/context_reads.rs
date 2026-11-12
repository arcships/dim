use super::*;
use dim_agent_runtime::TimelineQueryScope;
use dim_agent_runtime::testing::{chain_events, timeline_events};
use rutis::host::CtxHost;
use std::ops::RangeInclusive;

type DeferredPublication = (Vec<StoredEvent>, PublishCallback, Result<()>);

/// A memory store that counts whole-branch reads and by-id queries, records
/// the range of every event read and can fail, pause or defer chosen calls.
#[derive(Default)]
pub(super) struct CountContextReads {
    memory: MemoryTimelineStore,
    /// Reads that asked for the whole branch: a `read` or `read_segments`
    /// range with an open end. No path issues one.
    reads: AtomicUsize,
    /// By-id queries: the policy path looks up a compaction cut's position,
    /// and the streaming path does not.
    pub(super) positions: AtomicUsize,
    attempts: AtomicUsize,
    /// Each segment of every `read` and `read_segments`, in call order.
    pub(super) ranges: std::sync::Mutex<Vec<(String, RangeInclusive<u64>)>>,
    /// The branch and bounds of every query read, in call order, beside
    /// `ranges` because a rebuild reads through the query.
    pub(super) queries: std::sync::Mutex<Vec<(String, RangeInclusive<u64>)>>,
    /// The branch, cursor and number of events of every `read_page`, in
    /// call order.
    pub(super) pages: std::sync::Mutex<Vec<(String, u64, usize)>>,
    /// The kinds of every query, in call order.
    pub(super) query_kinds: std::sync::Mutex<Vec<Vec<String>>>,
    pub(super) property_queries: std::sync::Mutex<Vec<QuerySpec>>,
    /// Fails the given occurrence of a read of this branch and range with a
    /// storage error, counted from when it is set.
    pub(super) fail_range: std::sync::Mutex<Option<ReadOccurrence>>,
    /// Holds the given occurrence of a read of this branch and range:
    /// signals `entered`, then waits for `release`.
    pub(super) hold_range: std::sync::Mutex<Option<ReadOccurrence>>,
    /// Every page a context fold read, in call order.
    pub(super) context_pages: std::sync::Mutex<Vec<ContextPageRead>>,
    /// Input ids whose `input/consumed` rows this store hides from a query
    /// that asks for `data.input_id`: a partial prefix, which no public writer
    /// produces because a request consumes every pending input when it starts.
    pub(super) hidden_consumptions: std::sync::Mutex<Vec<String>>,
    fail_at: AtomicUsize,
    fail_configuration: std::sync::atomic::AtomicBool,
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

/// One page a context fold read: the branch it read, where it began, the skip
/// it asked for, and where it ended.
#[derive(Clone, Debug)]
pub(super) struct ContextPageRead {
    pub branch: String,
    pub from: u64,
    pub offset: u64,
    pub last: Option<u64>,
    pub first: Option<u64>,
}

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
    /// Fails or holds the occurrence set for a read of `range` of `branch`.
    async fn gate(&self, branch: &str, range: &RangeInclusive<u64>) -> Result<()> {
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
    /// Records a read of `range` of `branch`, then fails or holds it when it
    /// is the occurrence set for that.
    async fn record(&self, branch: &str, range: &RangeInclusive<u64>) -> Result<()> {
        if *range.end() == u64::MAX {
            self.reads.fetch_add(1, Ordering::SeqCst);
        }
        self.ranges
            .lock()
            .unwrap()
            .push((branch.to_owned(), range.clone()));
        let _ = TASK_READS
            .try_with(|reads| reads.borrow_mut().push((branch.to_owned(), range.clone())));
        self.gate(branch, range).await
    }
}
/// `result` without the `input/consumed` rows whose `data.input_id` is one of
/// `hidden`, and without those rows' page cursor positions. A page that hid
/// every row it had is not empty in the reply: the store's cursor still moves
/// past it, so the remainder of the chain is still read.
fn hide_consumptions(
    result: dim_agent_runtime::QueryResult,
    hidden: &[String],
) -> dim_agent_runtime::QueryResult {
    let QueryOutput::Rows { rows } = result.result else {
        return result;
    };
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            !row.values.iter().any(|value| {
                matches!(value, QueryValue::Value { value: serde_json::Value::String(input) }
                    if hidden.iter().any(|hidden| hidden == input))
            })
        })
        .collect();
    dim_agent_runtime::QueryResult {
        result: QueryOutput::Rows { rows },
        ..result
    }
}

impl TimelineStore for CountContextReads {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        Box::pin(async move {
            self.property_queries.lock().unwrap().push(query.clone());
            let mut filters: Vec<_> = query.filter.iter().collect();
            let mut kinds = Vec::new();
            while let Some(filter) = filters.pop() {
                match filter {
                    QueryFilter::In {
                        value: QueryOperand::Field { path },
                        values,
                    } if path == "kind" => kinds.extend(
                        values
                            .iter()
                            .map(|value| value.as_str().unwrap().to_owned()),
                    ),
                    QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                        filters.extend(conditions)
                    }
                    _ => {}
                }
            }
            if kinds == ["context/configured"]
                && self.fail_configuration.swap(false, Ordering::SeqCst)
            {
                return Err(AgentError::Storage(
                    "injected configuration read failure".into(),
                ));
            }
            self.query_kinds.lock().unwrap().push(kinds);
            for segment in &scope.segments {
                self.queries
                    .lock()
                    .unwrap()
                    .push((segment.branch_id.clone(), segment.from..=segment.through));
                self.gate(&segment.branch_id, &(segment.from..=segment.through))
                    .await?;
            }
            let result = self.memory.query(scope, query, page).await?;
            let hidden = self.hidden_consumptions.lock().unwrap().clone();
            let result = if hidden.is_empty() {
                result
            } else {
                hide_consumptions(result, &hidden)
            };
            if matches!(&query.filter, Some(QueryFilter::In { value: QueryOperand::Field { path }, .. }) if path == "id")
            {
                self.positions.fetch_add(1, Ordering::SeqCst);
            }
            if matches!(&query.filter, Some(QueryFilter::Or { conditions }) if conditions.iter().all(|condition| matches!(condition, QueryFilter::In { value: QueryOperand::Field { path }, .. } if path == "context.render" || path == "context.state")))
                && let QueryOutput::Events { events } = &result.result
            {
                for segment in &scope.segments {
                    let mut positions = events
                        .iter()
                        .filter(|row| row.position.branch_id == segment.branch_id)
                        .map(|row| row.position.sequence);
                    let first = positions.next();
                    let last = positions.next_back().or(first);
                    self.context_pages.lock().unwrap().push(ContextPageRead {
                        branch: segment.branch_id.clone(),
                        from: segment.from,
                        offset: 0,
                        first,
                        last,
                    });
                }
            }
            Ok(result)
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
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
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
async fn prepared_requests_use_saved_boundaries_and_growth_after_compaction_and_resume() {
    use dim_agent_capabilities::{context_preparation::ContextPreparation, prompt::Prompt};
    let store = Arc::new(CountContextReads::default());
    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(1_000.),
            ..Context::default()
        },
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .service::<Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(ModelSelection {
                provider_id: "fixture".into(),
                model_id: "counter".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    session
        .submit("x".repeat(1_200), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let usage = session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert!(
        usage.estimated_input_tokens >= usage.threshold_tokens,
        "the next preparation must test growth despite being above threshold: {usage:?}"
    );
    let first = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        first
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompactionStart(_)))
            .count(),
        1
    );
    store.context_pages.lock().unwrap().clear();
    session
        .submit("small".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        store.context_pages.lock().unwrap().len(),
        1,
        "growth and budget checks must not project a second history"
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompactionStart(_)))
            .count(),
        1,
        "too little new history must not trigger another compaction"
    );
    let budget = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextBudget(data) => Some(data),
            _ => None,
        })
        .unwrap();
    assert!(budget.estimated_input_tokens >= usage.threshold_tokens);
    let source = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextPrepare(data) if data.request_id == budget.request_id => {
                Some(data.through)
            }
            _ => None,
        })
        .unwrap();
    let history = HistoryRenderer::default()
        .context_in(&session, "main", source)
        .await
        .unwrap();
    assert_eq!(
        budget.boundary,
        project_context_history(&history)
            .unwrap()
            .last()
            .map(|entry| entry.id.clone())
    );
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    drop(session);
    store.context_pages.lock().unwrap().clear();
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent.restore_session(id).await.unwrap();
    session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert!(
        store.context_pages.lock().unwrap().is_empty(),
        "resume must reuse saved numeric boundaries and growth"
    );
    session
        .submit("z".repeat(800), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompactionStart(_)))
            .count(),
        2,
        "enough growth after resume must still trigger compaction"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn growth_checks_seek_numeric_boundaries_across_larger_restored_histories() {
    use dim_agent_capabilities::context_preparation::ContextPreparation;
    let mut reads = Vec::new();
    for size in [256usize, 2_048] {
        let store = Arc::new(CountContextReads::default());
        store
            .create_session("growth", &SessionConfig::default())
            .await
            .unwrap();
        let timeline = Timeline::new(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: "growth".into(),
                branch_id: "main".into(),
            },
            vec![],
            store.clone(),
        );
        let mut drafts = vec![
            EventDraft::new(EventKind::BranchStart(BranchStartData {
                parent: None,
                create_event: None,
            })),
            EventDraft::new(EventKind::ContextConfigured(ContextConfiguredData {
                timeline: "main".into(),
                revision: "fixture".into(),
                previous: None,
                prompt: None,
                settings: Box::new(ModelConfiguration {
                    provider_id: "fixture".into(),
                    model_id: "counter".into(),
                    capability_snapshot: None,
                    tools: Some(vec![]),
                    tool_sources: vec![],
                    config: None,
                    metadata: None,
                }),
                system: None,
                system_source: None,
            })),
        ];
        for n in 0..size {
            let id = format!("historic-{n}");
            drafts.push(
                EventDraft::new(EventKind::InputMessage(InputMessageData::text("x", None)))
                    .with_id(id.clone()),
            );
            drafts.push(EventDraft::new(EventKind::InputConsumed(
                InputConsumedData { input_id: id },
            )));
        }
        timeline
            .append_batch_registered(drafts, |_| Ok(()))
            .unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        let mut profile = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context {
                threshold: Some(100.),
                window: 1_000_000.,
                ..Context::default()
            },
        );
        profile.timeline_store(store.clone());
        let cell = Arc::new(std::sync::OnceLock::new());
        let capture = cell.clone();
        profile.require_feature(Feature::new("test.raw-timeline").branch(
            "test.raw-timeline",
            move |names| {
                Ok(Box::new(super::manual_compaction::RawTimeline::new(
                    names,
                    capture.clone(),
                )?))
            },
        ));
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent.restore_session("growth".into()).await.unwrap();
        session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        let timeline = cell.get().unwrap();
        let through = timeline.sequence();
        let start = timeline
            .append(
                EventKind::ContextCompactionStart(ContextCompactionStartData {
                    turn_id: Some("fixture".into()),
                    request_id: Some("attempt".into()),
                    command: None,
                    through,
                    trigger: CompactionTrigger::Threshold,
                    strategy: "test".into(),
                })
                .into(),
            )
            .unwrap();
        timeline
            .append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: Some("fixture".into()),
                    request_id: Some("attempt".into()),
                    command: None,
                    start: Some(start.sequence),
                    through,
                    outcome: ContextCompactionResult::Cancelled {
                        reason: "fixture settled".into(),
                    },
                })
                .into(),
            )
            .unwrap();
        agent.close().await.unwrap();
        drop(session);
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent.restore_session("growth".into()).await.unwrap();
        session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        store.context_pages.lock().unwrap().clear();
        store.property_queries.lock().unwrap().clear();
        session
            .submit("new".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        let mut last = None;
        for page in store.context_pages.lock().unwrap().iter() {
            if let (Some(first), Some(end)) = (page.first, page.last) {
                assert!(
                    last.is_none_or(|last| first > last),
                    "the policy reread a historical context range: {page:?}"
                );
                last = Some(end);
            }
        }
        let numeric_reads = store.property_queries.lock().unwrap().iter().filter(|query|
            matches!(&query.select, QuerySelect::Fields { fields } if fields == &["data.current.totals"]))
            .count();
        reads.push(numeric_reads);
        assert!(
            numeric_reads > 0,
            "this case must seek persisted numeric boundaries"
        );
        assert!(
            numeric_reads <= 12,
            "numeric growth must seek cumulative values without enumerating earlier results: {reads:?}"
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ContextCompactionStart(_)))
                .count(),
            1,
            "the saved range has less than a quarter-window of new history"
        );
        agent.close().await.unwrap();
    }
    assert!(
        reads[1] <= reads[0] + 4,
        "8x history should add at most a tree level: {reads:?}"
    );
    println!("numeric predecessor reads: {reads:?}");
}

#[tokio::test]
async fn persisted_usage_reads_and_restores_without_projecting_history() {
    for threshold in [None, Some(900.)] {
        let store = Arc::new(CountContextReads::default());
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context {
                threshold,
                ..Context::default()
            },
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let session_id = session.id().to_owned();
        session
            .service::<dim_agent_capabilities::prompt::Prompt>("main")
            .unwrap()
            .configure_loaded(
                Some(ModelSelection {
                    provider_id: "test".into(),
                    model_id: "test".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                vec![],
            )
            .await
            .unwrap();
        for text in ["first".repeat(256), "second".into()] {
            session
                .submit(text, InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let preparation = session
            .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
            .unwrap();
        let first = preparation.usage(CancellationToken::new()).await.unwrap();
        if threshold.is_some() {
            assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind,
            EventKind::ContextCompaction(data) if matches!(data.outcome, ContextCompactionResult::Completed { .. }))));
        }
        let history = HistoryRenderer::default()
            .context_in(&session, "main", first.through)
            .await
            .unwrap();
        let rendered = project_context_history(&history).unwrap();
        let messages = dim_agent_capabilities::history::context_messages(&history, &rendered);
        let old_body_ids: std::collections::HashSet<_> = history
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    EventKind::InputMessage(_)
                        | EventKind::ModelCall(_)
                        | EventKind::ModelContent(_)
                        | EventKind::Text(_)
                        | EventKind::Thinking(_)
                )
            })
            .map(|event| serde_json::Value::String(event.id.clone()))
            .collect();
        assert_eq!(
            first.estimated_input_tokens,
            serde_json::to_vec(&messages).unwrap().len() as f64
        );
        store.context_pages.lock().unwrap().clear();
        for _ in 0..3 {
            let next = preparation.usage(CancellationToken::new()).await.unwrap();
            assert_eq!(next.estimated_input_tokens, first.estimated_input_tokens);
        }
        assert!(
            store.context_pages.lock().unwrap().is_empty(),
            "usage queries projected settled history"
        );
        agent.close().await.unwrap();
        store.context_pages.lock().unwrap().clear();
        store.property_queries.lock().unwrap().clear();
        let mut resumed_builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context {
                threshold,
                window: 8192.,
                ..Context::default()
            },
        );
        resumed_builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&resumed_builder).await.unwrap();
        let restored = agent.restore_session(session_id.clone()).await.unwrap();
        let usage = restored
            .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(usage.estimated_input_tokens, first.estimated_input_tokens);
        assert_eq!(usage.context_window, 8192.);
        assert!(
            store.context_pages.lock().unwrap().is_empty(),
            "restoring saved usage projected settled history"
        );
        assert!(store.property_queries.lock().unwrap().iter().all(|query| {
        !matches!(&query.filter, Some(QueryFilter::In { value: QueryOperand::Field { path }, values })
            if path == "id" && values.iter().any(|id| old_body_ids.contains(id)))
    }), "restoring saved usage reread a settled source object");
        agent.close().await.unwrap();

        let timeline = Timeline::restore(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: session_id.clone(),
                branch_id: "main".into(),
            },
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let input = timeline
            .append(
                EventKind::InputMessage(InputMessageData::text(
                    "new after saved result",
                    Some(InputMode::Append),
                ))
                .into(),
            )
            .unwrap();
        timeline
            .append(
                EventKind::InputConsumed(InputConsumedData {
                    input_id: input.id.clone(),
                })
                .into(),
            )
            .unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        store.context_pages.lock().unwrap().clear();
        store.property_queries.lock().unwrap().clear();
        let agent = MinimalHost::start(&resumed_builder).await.unwrap();
        let restored = agent.restore_session(session_id).await.unwrap();
        let usage = restored
            .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            usage.estimated_input_tokens,
            first.estimated_input_tokens
                + Context::default()
                    .message(&Message::text(Role::User, "new after saved result"))
                    .serialized_bytes as f64
        );
        assert!(
            store.context_pages.lock().unwrap().is_empty(),
            "processing a new source replayed settled history"
        );
        assert!(store.property_queries.lock().unwrap().iter().all(|query| {
        !matches!(&query.filter, Some(QueryFilter::In { value: QueryOperand::Field { path }, values })
            if path == "id" && values.iter().any(|id| old_body_ids.contains(id)))
    }), "processing a new source reread a settled source object");
        agent.close().await.unwrap();
    }
}

struct CompleteRequestOnly;
impl ContextModel for CompleteRequestOnly {
    fn context_window(&self) -> f64 {
        4096.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        10_000.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        Context::default().count_tokens(request)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_during_usage_result_publication_is_not_a_statistics_fault() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (publication, mut pending) =
            tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let publisher = {
            let (entered, release) = (entered.clone(), release.clone());
            tokio::spawn(async move {
                while let Some((events, publish, result)) = pending.recv().await {
                    if events.iter().any(|stored| {
                        matches!(&stored.event.kind,
                        EventKind::Extension(data) if data.key() == "context-usage/recovered")
                    }) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    publish(result);
                }
            })
        };
        let store = Arc::new(CountContextReads {
            publication: Some(publication),
            ..Default::default()
        });
        let mut builder = builder(Arc::new(EchoModel));
        builder.timeline_store(store);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .service::<dim_agent_capabilities::prompt::Prompt>("main")
            .unwrap()
            .configure_loaded(
                Some(ModelSelection {
                    provider_id: "test".into(),
                    model_id: "test".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                vec![],
            )
            .await
            .unwrap();
        entered.notified().await;
        let closed = tokio::spawn(agent.close());
        while !session.is_closing() {
            tokio::task::yield_now().await;
        }
        release.notify_one();
        closed.await.unwrap().unwrap();
        publisher.abort();
    })
    .await
    .unwrap();
}
impl ResolveContextModel for CompleteRequestOnly {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async { Ok(Arc::new(Self) as Arc<dyn ContextModel>) })
    }
}

fn builder_with_usage_resolver(resolver: Arc<dyn ResolveContextModel>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(resolver))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(
            Context::default(),
        )))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}

#[derive(Default)]
struct AvailableCounter(std::sync::atomic::AtomicBool);
impl ResolveContextModel for AvailableCounter {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async {
            if self.0.load(Ordering::SeqCst) {
                Ok(Arc::new(Context::default()) as Arc<dyn ContextModel>)
            } else {
                Err(HostError::failed("counter model unavailable"))
            }
        })
    }
}

#[tokio::test]
async fn unavailable_usage_counter_does_not_fault_resume_and_can_recover() {
    let store = Arc::new(CountContextReads::default());
    let resolver = Arc::new(AvailableCounter::default());
    resolver.0.store(true, Ordering::SeqCst);
    let mut builder = builder_with_usage_resolver(resolver.clone());
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    session
        .service::<dim_agent_capabilities::prompt::Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(ModelSelection {
                provider_id: "test".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    session
        .submit("saved input".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let expected = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    agent.close().await.unwrap();

    resolver.0.store(false, Ordering::SeqCst);
    store.context_pages.lock().unwrap().clear();
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session(id.clone()).await.unwrap();
    let unavailable = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await;
    assert!(
        matches!(unavailable, Err(dim_agent_capabilities::context_preparation::ContextOpError::Model(error))
        if error.to_string() == "counter model unavailable")
    );
    assert!(
        store.context_pages.lock().unwrap().is_empty(),
        "unavailable counter projected historical messages"
    );
    agent.close().await.unwrap();

    resolver.0.store(true, Ordering::SeqCst);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session(id).await.unwrap();
    let restored = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        restored.estimated_input_tokens,
        expected.estimated_input_tokens
    );
    store.context_pages.lock().unwrap().clear();
    session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert!(
        store.context_pages.lock().unwrap().is_empty(),
        "recovered counter repeatedly initialized history"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_usage_counter_does_not_fall_back_to_history_projection() {
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder_with_usage_resolver(Arc::new(CompleteRequestOnly));
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .service::<dim_agent_capabilities::prompt::Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(ModelSelection {
                provider_id: "test".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    store.context_pages.lock().unwrap().clear();
    let result = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await;
    assert!(
        matches!(result, Err(dim_agent_capabilities::context_preparation::ContextOpError::Model(error))
        if error.to_string().contains("does not support persistent usage"))
    );
    assert!(
        store.context_pages.lock().unwrap().is_empty(),
        "unsupported usage reconstructed history"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn unconfigured_usage_does_not_project_legacy_messages() {
    let store = Arc::new(CountContextReads::default());
    store
        .create_session("unconfigured", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "unconfigured".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    timeline
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: None,
                create_event: None,
            })
            .into(),
        )
        .unwrap();
    for _ in 0..256 {
        let input = timeline
            .append(EventKind::InputMessage(InputMessageData::text("x".repeat(4096), None)).into())
            .unwrap();
        timeline
            .append(
                EventKind::InputConsumed(InputConsumedData {
                    input_id: input.id.clone(),
                })
                .into(),
            )
            .unwrap();
    }
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "legacy".into(),
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::ModelStart(ModelStartData {
                turn_id: "legacy".into(),
                request_id: "legacy".into(),
            })
            .into(),
        )
        .unwrap();
    let call = timeline
        .append(
            EventKind::ModelCall(ModelCallData {
                turn_id: "legacy".into(),
                request_id: "legacy".into(),
                request: Arc::new(ModelRequest::new(
                    "fixture",
                    "model",
                    (0..256)
                        .map(|_| Message::text(Role::User, "x".repeat(4096)))
                        .collect(),
                )),
                configuration: None,
                prompt: None,
                failure_chain: None,
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: "legacy".into(),
                request_id: "legacy".into(),
                outcome: ModelOutputOutcome::Completed {
                    finish_reason: FinishReason::EndTurn,
                },
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::TurnEnd(TurnEndData {
                turn_id: "legacy".into(),
                outcome: TurnOutcome::Completed {
                    reason: "fixture completed".into(),
                },
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("unconfigured".into()).await.unwrap();
    let preparation = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap();
    store.context_pages.lock().unwrap().clear();
    let reply = preparation.usage(CancellationToken::new()).await;
    assert!(
        matches!(
            reply,
            Err(dim_agent_capabilities::context_preparation::ContextOpError::NotConfigured)
        ),
        "{reply:?}"
    );
    assert!(
        store.context_pages.lock().unwrap().is_empty(),
        "an unavailable usage read must not reconstruct legacy context"
    );
    assert!(store.property_queries.lock().unwrap().iter().all(|query| {
        !matches!(&query.filter, Some(QueryFilter::In { value: QueryOperand::Field { path }, values })
            if path == "id" && values.contains(&serde_json::Value::String(call.id.clone())))
    }), "an unconfigured read loaded a call without a provider measurement");
    agent.close().await.unwrap();
}

/// `/usage` reads what the branch has published: a fact that holds its
/// sequence but is not published yet is not a read point the policy refuses,
/// and the usage is answered while it is in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_reads_the_published_head_while_a_fact_is_in_flight() {
    let (publication, mut publications) =
        tokio::sync::mpsc::unbounded_channel::<DeferredPublication>();
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let in_flight = Arc::new(tokio::sync::Notify::new());
    let held_publication = {
        let (hold, in_flight) = (hold.clone(), in_flight.clone());
        tokio::spawn(async move {
            while let Some((_, publish, result)) = publications.recv().await {
                while hold.load(Ordering::SeqCst) {
                    in_flight.notify_one();
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                publish(result);
            }
        })
    };
    let store = Arc::new(CountContextReads {
        publication: Some(publication),
        ..CountContextReads::default()
    });
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
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    hold.store(true, Ordering::SeqCst);
    session.interrupt("main", "usage read".into()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), in_flight.notified())
        .await
        .expect("a fact is in flight");
    let preparation = session
        .service::<dim_agent_capabilities::context_preparation::ContextPreparation>("main")
        .unwrap();
    let usage = preparation.usage(CancellationToken::new()).await;
    hold.store(false, Ordering::SeqCst);
    assert!(
        !matches!(
            usage,
            Err(dim_agent_capabilities::context_preparation::ContextOpError::Read(_))
        ),
        "an in-flight fact made the usage a refused read: {usage:?}"
    );
    agent.close().await.unwrap();
    held_publication.abort();
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
        let recovered = tokio::time::timeout(
            Duration::from_secs(1),
            timeline_events(&timeline, TimelineQueryScope::Branch, ..=through),
        )
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
            timeline.batch_end(2).await,
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
        assert!(chain_events(&session, "main", ..).await.unwrap().iter().all(|event| {
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
        let first_turn = chain_events(&session, "main", ..).await.unwrap();
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
        assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(|event| {
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
        assert!(!chain_events(&session, "main", ..).await.unwrap().iter()
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
        assert_eq!(chain_events(&session, "main", ..).await.unwrap().iter()
            .filter(|event| matches!(event.kind, EventKind::TurnEnd(_))).count(), 1);
        session.submit("after stop".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        assert!(chain_events(&session, "main", ..).await.unwrap().iter()
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
        let failed = chain_events(&session, "main", ..).await.unwrap();
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
        let recovered = chain_events(&session, "main", ..).await.unwrap();
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
            let positions = store.positions.load(Ordering::SeqCst);
            session
                .submit(input, InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            // With a compaction policy in force every model step looks up the
            // position of the compaction it finds; without one the context is
            // only ever streamed in bounded batches. Neither reads the whole
            // branch.
            let reads = store.reads.load(Ordering::SeqCst) - before;
            assert_eq!(
                reads, 0,
                "policy={policy}; a model step read the whole branch"
            );
            let positions = store.positions.load(Ordering::SeqCst) - positions;
            if policy {
                assert!(
                    positions >= 1,
                    "policy=true; a model step looked up no cut position"
                );
            } else {
                assert_eq!(
                    positions, 0,
                    "policy=false; the streaming path looked up a cut position"
                );
            }
        }
        let history = chain_events(&session, "main", ..).await.unwrap();
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
                store.context_pages.lock().unwrap().clear();
                let (recovered, _) = renderer
                    .rendered_context(&timeline, timeline.sequence())
                    .await
                    .unwrap();
                assert!(recovered.checkpoint.is_some());
                assert_eq!(
                    store
                        .context_pages
                        .lock()
                        .unwrap()
                        .first()
                        .map(|page| page.from),
                    Some(checkpoint.resume.sequence),
                    "the fold begins at the resume event"
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
    let history = chain_events(&session, "main", ..).await.unwrap();
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
    store.context_pages.lock().unwrap().clear();
    let (recovered, restored) = renderer
        .rendered_context(&timeline, timeline.sequence())
        .await
        .unwrap();
    assert!(
        recovered.checkpoint.is_some(),
        "the restore resumes from a checkpoint"
    );
    assert_eq!(
        store
            .context_pages
            .lock()
            .unwrap()
            .first()
            .map(|page| page.from),
        Some(checkpoint.resume.sequence),
        "the fold begins at the resume event"
    );
    let full = project_context_history(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, timeline.sequence())
            .await
            .unwrap(),
    )
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
                let interrupt = chain_events(&session, "main", ..)
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
        let events = chain_events(&session, "main", ..).await.unwrap();
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
async fn configuration_reference_read_failure_aborts_resume_and_allows_retry() {
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
    store.fail_configuration.store(true, Ordering::SeqCst);
    assert_eq!(
        session.configure(update, "main").await.unwrap(),
        ConfigureOutcome::Adopted
    );
    assert!(
        store.fail_configuration.load(Ordering::SeqCst),
        "live adoption must use its current configuration instead of querying history"
    );
    assert!(session.failure().is_none());
    let id = session.id().to_owned();
    agent.close().await.unwrap();

    let agent = MinimalHost::start(&builder).await.unwrap();
    let failed = match agent.restore_session(id.clone()).await {
        Ok(_) => panic!("a missing configuration read must prevent publication"),
        Err(error) => error,
    };
    assert!(
        failed
            .to_string()
            .contains("injected configuration read failure"),
        "{failed}"
    );
    assert!(!store.fail_configuration.load(Ordering::SeqCst));
    agent.close().await.unwrap();

    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session(id).await.unwrap();
    session
        .submit("after recovery retry".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextConfigured(_)))
            .count(),
        1,
        "failed recovery and retry must preserve the original adoption"
    );
    assert!(history.iter().any(|event|
        matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after recovery retry"))));
    let request = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert!(
        serde_json::to_string(&request.system)
            .unwrap()
            .contains("alpha:1")
    );
    agent.close().await.unwrap();
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
        let events = chain_events(&session, "main", ..).await.unwrap();
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
            let events = chain_events(&session, "main", ..).await.unwrap();
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
    let events = chain_events(&session, "main", ..).await.unwrap();
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
            !chain_events(&session, "main", ..)
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
            chain_events(&session, "main", ..)
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
    let events = chain_events(&session, "main", ..).await.unwrap();
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

/// A preparation that stays below the threshold reads no turn order and no
/// cut window: its estimate takes one page per fact it compares against, so
/// what it reads does not grow with the stored history (#914).
#[tokio::test]
async fn a_below_threshold_preparation_reads_no_growing_turn_order() {
    let store = Arc::new(CountContextReads::default());
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(1_000_000.),
            ..Context::default()
        },
    );
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let mut per_turn: Vec<Vec<Vec<String>>> = Vec::new();
    for index in 0..12 {
        let before = store.query_kinds.lock().unwrap().len();
        session
            .submit(format!("turn {index}"), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let calls = store.query_kinds.lock().unwrap()[before..].to_vec();
        per_turn.push(calls);
    }
    for calls in &per_turn {
        for kinds in calls {
            assert!(
                !kinds.iter().any(|kind| kind == "turn/start"),
                "a below-threshold preparation reads no turn order: {kinds:?}"
            );
        }
    }
    assert_eq!(
        per_turn[0].len(),
        per_turn[per_turn.len() - 1].len(),
        "the reads of a turn do not grow with the stored history: {per_turn:?}"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn an_ancestor_fold_continues_without_duplicate_events_at_a_fixed_cutoff() {
    let store = Arc::new(CountContextReads::default());
    store
        .create_session("fold", &SessionConfig::default())
        .await
        .unwrap();
    let parent = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "fold".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let mut facts = Vec::new();
    for index in 0..6 {
        let input = parent
            .append(
                EventKind::InputMessage(InputMessageData::text(format!("parent {index}"), None))
                    .into(),
            )
            .unwrap();
        facts.push(input.clone());
        facts.push(
            parent
                .append(
                    EventKind::InputConsumed(InputConsumedData {
                        input_id: input.id.clone(),
                    })
                    .into(),
                )
                .unwrap(),
        );
    }
    let cut = parent.sequence();
    parent
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: BranchSource::Host {
                    operation_id: "test-branch".into(),
                },
                branch: "child".into(),
                prompt: "child".into(),
            })
            .into(),
        )
        .unwrap();
    let child = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "fold".into(),
            branch_id: "child".into(),
        },
        vec![("main".into(), cut)],
        store.clone(),
    );
    for index in 0..2 {
        let input = child
            .append(
                EventKind::InputMessage(InputMessageData::text(format!("child {index}"), None))
                    .into(),
            )
            .unwrap();
        child
            .append(
                EventKind::InputConsumed(InputConsumedData {
                    input_id: input.id.clone(),
                })
                .into(),
            )
            .unwrap();
    }
    let through = child.sequence();
    let renderer = HistoryRenderer::default();
    let (_, full) = renderer.rendered_context(&child, through).await.unwrap();
    store.context_pages.lock().unwrap().clear();
    let streamed = renderer.rendered_stream(&child, through, 2).await.unwrap();
    assert_eq!(streamed.history, full);
    let pages: Vec<_> = store
        .context_pages
        .lock()
        .unwrap()
        .iter()
        .filter(|page| page.branch == "main" && page.first.is_some())
        .cloned()
        .collect();
    assert!(
        pages.len() > 1,
        "the ancestor prefix is read in more than one page: {pages:?}"
    );
    assert!(
        pages.iter().all(|page| page.offset == 0),
        "an ancestor page starts at an event, not at a growing skip: {pages:?}"
    );
    assert_eq!(
        pages.first().map(|page| page.from),
        facts.first().map(|event| event.sequence),
        "the first ancestor page begins at the branch's first fact: {pages:?}"
    );
    for pair in pages.windows(2) {
        assert!(
            pair[1].first > pair[0].last,
            "continued pages do not return events from the preceding page: {pages:?}"
        );
        assert_eq!(
            pair[1].from, pair[0].from,
            "continuation preserves the read scope"
        );
    }
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&parent)
        .await
        .unwrap();
}

/// A streamed context resumes at the event its checkpoint names
/// ([ADR 020](../../../../docs/adr/020-timeline-read-convergence.md) D5): the
/// fold reads the chain from that event, never from the chain's start, and its
/// projection is the one the full render gives.
#[tokio::test]
async fn a_streamed_context_resumes_at_the_event_its_checkpoint_names() {
    use super::checkpoint_projection::completed;

    let store = Arc::new(CountContextReads::default());
    store
        .create_session("resume", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "resume".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let append = |kind: EventKind| timeline.append(kind.into()).unwrap();
    let first = append(EventKind::InputMessage(InputMessageData::text(
        "first", None,
    )));
    append(EventKind::InputConsumed(InputConsumedData {
        input_id: first.id.clone(),
    }));
    let cut = append(EventKind::TurnStart(TurnStartData {
        turn_id: "turn-1".into(),
    }));
    let kept = append(EventKind::InputMessage(InputMessageData::text(
        "kept", None,
    )));
    append(EventKind::InputConsumed(InputConsumedData {
        input_id: kept.id.clone(),
    }));
    let through = timeline.sequence();
    let renderer = HistoryRenderer::default();
    let fact = append(completed(&cut.id, "summary", through, vec![]));
    let saved = renderer
        .snapshot(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, through)
                .await
                .unwrap(),
            &cut.id,
            &fact,
        )
        .unwrap();
    timeline.save_snapshot(saved).await.unwrap();
    let resume = timeline
        .checkpoint(
            fact.sequence,
            renderer.checkpoint_interpreter,
            renderer.decode_checkpoint,
        )
        .await
        .unwrap()
        .expect("the checkpoint is usable")
        .resume;
    assert!(resume.sequence > 1, "the resume is inside the branch");

    let (_, full) = renderer
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    store.context_pages.lock().unwrap().clear();
    let streamed = renderer
        .rendered_stream(&timeline, fact.sequence, 8)
        .await
        .unwrap();
    assert_eq!(streamed.history, full);
    let pages = store.context_pages.lock().unwrap().clone();
    assert_eq!(
        pages.first().map(|page| page.from),
        Some(resume.sequence),
        "the fold begins at the resume event: {pages:?}"
    );
    assert!(
        pages.iter().all(|page| page.from >= resume.sequence),
        "{pages:?}"
    );
}

/// A boundary check after a completed compaction folds only the events after
/// its cut, and its verdict is the one the whole prefix gives. A cut whose
/// tail cannot start the state fold (a consumed input's event) falls back to
/// the segment start, and a cut inside an atomic batch is still refused.
#[tokio::test]
async fn a_cut_after_a_compaction_reads_only_its_tail_and_keeps_the_verdict() {
    use super::checkpoint_projection::completed;

    let store = Arc::new(CountContextReads::default());
    store
        .create_session("reads", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "reads".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let append = |kind: EventKind| timeline.append(kind.into()).unwrap();
    let first = append(EventKind::InputMessage(InputMessageData::text(
        "first", None,
    )));
    append(EventKind::InputConsumed(InputConsumedData {
        input_id: first.id.clone(),
    }));
    append(EventKind::TurnStart(TurnStartData {
        turn_id: "turn-1".into(),
    }));
    append(EventKind::ModelStart(ModelStartData {
        turn_id: "turn-1".into(),
        request_id: "request-1".into(),
    }));
    let early_text = append(EventKind::Text(TextData {
        turn_id: "turn-1".into(),
        request_id: "request-1".into(),
        delta: "hello".into(),
    }));
    append(EventKind::ModelOutputEnd(ModelOutputEndData {
        turn_id: "turn-1".into(),
        request_id: "request-1".into(),
        outcome: ModelOutputOutcome::Completed {
            finish_reason: FinishReason::EndTurn,
        },
    }));
    append(EventKind::TurnEnd(TurnEndData {
        turn_id: "turn-1".into(),
        outcome: TurnOutcome::Completed {
            reason: "end_turn".into(),
        },
    }));
    let second = append(EventKind::InputMessage(InputMessageData::text(
        "second", None,
    )));
    let consumed = append(EventKind::InputConsumed(InputConsumedData {
        input_id: second.id.clone(),
    }));
    let cut = append(EventKind::TurnStart(TurnStartData {
        turn_id: "turn-2".into(),
    }));
    let compaction = append(completed(&cut.id, "short", cut.sequence, vec![]));
    append(EventKind::ModelStart(ModelStartData {
        turn_id: "turn-2".into(),
        request_id: "request-2".into(),
    }));
    let call = append(EventKind::ToolUseStart(ToolUseStartData {
        turn_id: "turn-2".into(),
        request_id: "request-2".into(),
        call_id: "call-1".into(),
        name: "echo".into(),
    }));
    append(EventKind::ToolUse(ToolUseData {
        turn_id: "turn-2".into(),
        request_id: "request-2".into(),
        call: ToolCall {
            id: "call-1".into(),
            name: "echo".into(),
            input: JsonObject::new(),
        },
    }));
    let closed = append(EventKind::ModelOutputEnd(ModelOutputEndData {
        turn_id: "turn-2".into(),
        request_id: "request-2".into(),
        outcome: ModelOutputOutcome::Completed {
            finish_reason: FinishReason::ToolUse,
        },
    }));
    let result = append(EventKind::ToolResult(ToolResultData {
        turn_id: "turn-2".into(),
        request_id: "request-2".into(),
        call_event_id: Some(call.id.clone()),
        results: vec![ToolCallResult {
            call_id: "call-1".into(),
            result: ToolResult::text("done"),
        }],
    }));
    let end = append(EventKind::TurnEnd(TurnEndData {
        turn_id: "turn-2".into(),
        outcome: TurnOutcome::Completed {
            reason: "end_turn".into(),
        },
    }));

    for at in [
        0,
        early_text.sequence,
        cut.sequence,
        compaction.sequence,
        closed.sequence,
        result.sequence,
        end.sequence,
    ] {
        let expected = dim_agent_capabilities::history::validate_context_boundary(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, at)
                .await
                .unwrap(),
        );
        let actual = dim_agent_capabilities::history::validate_context_boundary_at(&timeline, at)
            .await
            .unwrap();
        assert_eq!(actual, expected, "cut {at}");
    }

    store.context_pages.lock().unwrap().clear();
    assert_eq!(
        dim_agent_capabilities::history::validate_context_boundary_at(&timeline, end.sequence)
            .await
            .unwrap(),
        Ok(())
    );
    let pages = store.context_pages.lock().unwrap().clone();
    assert_eq!(
        pages.first().map(|page| (page.branch.as_str(), page.from)),
        Some(("main", cut.sequence)),
        "{pages:?}"
    );
    assert!(
        pages.iter().all(|page| page.from >= cut.sequence),
        "{pages:?}"
    );

    assert_eq!(
        dim_agent_capabilities::history::validate_context_boundary_at(&timeline, consumed.sequence)
            .await
            .unwrap(),
        Ok(())
    );

    // A completed compaction whose cut replaced the input a later
    // `input/consumed` names cannot start the state fold; the check falls back
    // to the segment start and still gives the verdict.
    let fallback = append(completed(&consumed.id, "second", end.sequence, vec![]));
    store.context_pages.lock().unwrap().clear();
    assert_eq!(
        dim_agent_capabilities::history::validate_context_boundary_at(&timeline, fallback.sequence)
            .await
            .unwrap(),
        Ok(())
    );
    let pages = store.context_pages.lock().unwrap().clone();
    assert_eq!(
        pages.first().map(|page| page.from),
        Some(consumed.sequence),
        "{pages:?}"
    );
    assert_eq!(pages.last().map(|page| page.from), Some(1), "{pages:?}");

    let batch = timeline
        .append_batch_registered(
            vec![
                EventKind::InputMessage(InputMessageData::text("batch", None)).into(),
                EventKind::InputMessage(InputMessageData::text("batch", None)).into(),
            ],
            |_| Ok(()),
        )
        .unwrap();
    store.ranges.lock().unwrap().clear();
    assert!(matches!(
        dim_agent_capabilities::history::validate_context_boundary_at(&timeline, batch[0].sequence)
            .await,
        Err(AgentError::InvalidInput(message)) if message.contains("inside an atomic batch")
    ));
    assert!(
        store.ranges.lock().unwrap().is_empty(),
        "the batch-edge check reads the stored row, not the event"
    );
    assert_eq!(
        dim_agent_capabilities::history::validate_context_boundary_at(&timeline, batch[1].sequence)
            .await
            .unwrap(),
        Ok(()),
        "the batch's last sequence ends it and may be a cut"
    );
}

/// A stored cut that names a later event, or lies beyond `through`, is ignored:
/// the check replays the segment instead of accepting the empty state of a tail
/// it never read.
#[tokio::test]
async fn a_malformed_compaction_cut_replays_the_segment_and_keeps_the_verdict() {
    use super::checkpoint_projection::completed;

    let store = Arc::new(CountContextReads::default());
    store
        .create_session("reads", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "reads".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "turn-1".into(),
            })
            .into(),
        )
        .unwrap();
    let open = timeline
        .append(
            EventKind::ModelStart(ModelStartData {
                turn_id: "turn-1".into(),
                request_id: "request".into(),
            })
            .into(),
        )
        .unwrap();
    let later = "later-turn";
    timeline
        .append(completed(later, "short", open.sequence, vec![]).into())
        .unwrap();
    let turn = timeline
        .append(
            EventDraft::new(EventKind::TurnStart(TurnStartData {
                turn_id: "turn-2".into(),
            }))
            .with_id(later),
        )
        .unwrap();

    for at in [open.sequence + 1, turn.sequence] {
        let expected = dim_agent_capabilities::history::validate_context_boundary(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, at)
                .await
                .unwrap(),
        );
        assert!(
            expected.is_err(),
            "the full prefix leaves the model output open at {at}"
        );
        assert_eq!(
            dim_agent_capabilities::history::validate_context_boundary_at(&timeline, at)
                .await
                .unwrap(),
            expected,
            "cut {at}"
        );
    }
}

#[derive(Clone, Default)]
struct ObservedCounter {
    fixed: Arc<AtomicUsize>,
    messages: Arc<AtomicUsize>,
    full: Arc<AtomicUsize>,
    fixed_value: Arc<std::sync::Mutex<(u64, Vec<String>)>>,
}
impl ResolveContextModel for ObservedCounter {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        let model = self.clone();
        Box::pin(async move { Ok(Arc::new(model) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for ObservedCounter {
    fn context_window(&self) -> f64 {
        4096.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        10_000.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        self.full.fetch_add(1, Ordering::SeqCst);
        serde_json::to_vec(&(request.system.as_ref(), request.tools.as_ref()))
            .unwrap()
            .len() as f64
            + request
                .messages
                .iter()
                .map(|message| serde_json::to_vec(message).unwrap().len() as f64)
                .sum::<f64>()
    }
    fn contribution_counter(&self) -> Option<&dyn ContextCounter> {
        Some(self)
    }
}
impl ContextCounter for ObservedCounter {
    fn algorithm(&self) -> &str {
        "test/observed-additive/v1"
    }
    fn fixed(
        &self,
        system: Option<&SystemPrompt>,
        tools: Option<&[ToolDefinition]>,
    ) -> ContextContribution {
        self.fixed.fetch_add(1, Ordering::SeqCst);
        let bytes = serde_json::to_vec(&(system, tools)).unwrap().len() as u64;
        *self.fixed_value.lock().unwrap() = (
            bytes,
            tools
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        );
        ContextContribution {
            serialized_bytes: bytes,
            images: 0,
        }
    }
    fn message(&self, message: &Message) -> ContextContribution {
        self.messages.fetch_add(1, Ordering::SeqCst);
        ContextContribution {
            serialized_bytes: serde_json::to_vec(message).unwrap().len() as u64,
            images: 0,
        }
    }
    fn estimate(&self, contribution: ContextContribution) -> f64 {
        contribution.serialized_bytes as f64
    }
}

#[tokio::test]
async fn usage_reuses_counted_messages_and_current_registry_tools_without_extra_state_writes() {
    use dim_agent_capabilities::context_preparation::ContextPreparation;
    let counter = Arc::new(ObservedCounter::default());
    let mut profile = builder_with_usage_resolver(counter.clone());
    profile.timeline_store(Arc::new(MemoryTimelineStore::default()));
    let cell = Arc::new(std::sync::OnceLock::new());
    let capture = cell.clone();
    profile.require_feature(Feature::new("test.raw-timeline").branch(
        "test.raw-timeline",
        move |names| {
            Ok(Box::new(super::manual_compaction::RawTimeline::new(
                names,
                capture.clone(),
            )?))
        },
    ));
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig {
            tools: Some(vec!["echo".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    let timeline = cell.get().unwrap();
    timeline
        .append(
            EventKind::ContextConfigured(ContextConfiguredData {
                timeline: "main".into(),
                revision: "fixture".into(),
                previous: None,
                prompt: None,
                settings: Box::new(ModelConfiguration {
                    provider_id: "fixture".into(),
                    model_id: "counter".into(),
                    capability_snapshot: None,
                    tools: None,
                    tool_sources: vec![],
                    config: None,
                    metadata: None,
                }),
                system: None,
                system_source: None,
            })
            .into(),
        )
        .unwrap();
    let preparation = session.service::<ContextPreparation>("main").unwrap();
    let empty = preparation.usage(CancellationToken::new()).await.unwrap();
    assert_eq!(
        empty.estimated_input_tokens,
        counter.fixed_value.lock().unwrap().0 as f64
    );
    assert!(
        counter
            .fixed_value
            .lock()
            .unwrap()
            .1
            .iter()
            .any(|name| name == "echo"),
        "implicit tools include the current registry declarations"
    );
    let input = timeline
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "one adopted input",
                Some(InputMode::Append),
            ))
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: input.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let value = preparation.usage(CancellationToken::new()).await.unwrap();
    assert!(value.estimated_input_tokens > empty.estimated_input_tokens);
    assert_eq!(
        counter.messages.load(Ordering::SeqCst),
        1,
        "a newly adopted message is counted once"
    );
    let calls = (
        counter.fixed.load(Ordering::SeqCst),
        counter.messages.load(Ordering::SeqCst),
        counter.full.load(Ordering::SeqCst),
    );
    let before = chain_events(&session, "main", ..).await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            preparation
                .usage(CancellationToken::new())
                .await
                .unwrap()
                .estimated_input_tokens,
            value.estimated_input_tokens
        );
    }
    let after = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(after.len(), before.len(), "ready reads do not write state");
    assert_eq!(
        (
            counter.fixed.load(Ordering::SeqCst),
            counter.messages.load(Ordering::SeqCst),
            counter.full.load(Ordering::SeqCst)
        ),
        calls,
        "ready reads reuse both fixed and message contributions"
    );
    assert_eq!(after.iter().filter(|event|matches!(&event.kind,EventKind::Extension(data) if data.key()=="context-usage/recovered")).count(),2,"configuration and the adopted input each save one numeric result");
    assert!(after.iter().all(|event|!matches!(&event.kind,EventKind::Extension(data) if data.key().starts_with("input-queue/")||data.key()=="context-usage/node"||data.key()=="context-usage/request-base")),"no queue snapshots, numeric nodes or request facts");
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let messages = counter.messages.load(Ordering::SeqCst);
    let agent = MinimalHost::start(&profile).await.unwrap();
    let restored = agent.restore_session(id).await.unwrap();
    let reading = restored
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        reading.estimated_input_tokens,
        counter.fixed_value.lock().unwrap().0 as f64
            + serde_json::to_vec(&Message::text(Role::User, "one adopted input"))
                .unwrap()
                .len() as f64,
        "reload uses the adopted guide and current tool controls without recounting the input"
    );
    assert_eq!(
        counter.messages.load(Ordering::SeqCst),
        messages,
        "reload does not recount settled messages"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn usage_upgrades_saved_numbers_or_initializes_once_without_historical_result_backfill() {
    use dim_agent_capabilities::{context_preparation::ContextPreparation, prompt::Prompt};
    use serde_json::{Value, json};
    let source = Arc::new(MemoryTimelineStore::default());
    let counter = Arc::new(ObservedCounter::default());
    let mut profile = builder_with_usage_resolver(counter.clone());
    profile.timeline_store(source.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    session
        .service::<Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(ModelSelection {
                provider_id: "test".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    for text in ["first saved input", "second saved input"] {
        session
            .submit(text.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    let expected = session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    agent.close().await.unwrap();
    let key = TimelineKey {
        session_id: id.clone(),
        branch_id: "main".into(),
    };
    let original = source
        .read(&key, 1..=source.head(&key).await.unwrap())
        .await
        .unwrap();
    let points: Vec<Value> = original
        .iter()
        .filter_map(|record| match &record.event.kind {
            EventKind::Extension(data) if data.key() == "context-usage/recovered" => {
                Some(data.data().clone())
            }
            _ => None,
        })
        .collect();
    let initial_history = project_history(
        &original
            .iter()
            .map(|record| record.event.clone())
            .collect::<Vec<_>>(),
        "main",
    )
    .unwrap();
    let fixed = expected.estimated_input_tokens
        - initial_history
            .iter()
            .map(|entry| serde_json::to_vec(&entry.message).unwrap().len() as f64)
            .sum::<f64>();
    let latest = points.last().unwrap();
    let mut previous = json!({"ordinary":{"serialized_bytes":0,"images":0},"growth":0});
    let mut entries = Vec::new();
    for point in &points {
        let mut totals = point["current"]["totals"].clone();
        if totals.is_null() {
            continue;
        }
        totals.as_object_mut().unwrap().remove("message_count");
        if let Some(ready) = totals
            .pointer_mut("/configuration/counter")
            .and_then(Value::as_object_mut)
        {
            ready.remove("control_identity");
            ready.remove("configured_tools");
        }
        let mut delta = totals.clone();
        for field in ["serialized_bytes", "images"] {
            delta["ordinary"][field] = (totals["ordinary"][field].as_u64().unwrap()
                - previous["ordinary"][field].as_u64().unwrap())
            .into();
        }
        delta["growth"] = (totals["growth"].as_f64().unwrap_or(0.)
            - previous["growth"].as_f64().unwrap_or(0.))
        .into();
        entries.push(json!({"at":point["current"]["at"],"summary":delta}));
        previous = totals;
    }
    for schema in [
        None,
        Some("context-usage/v1"),
        Some("context-usage/v2"),
        Some("context-usage/v3"),
        Some("context-usage/v4"),
    ] {
        let store = Arc::new(CountContextReads::default());
        store
            .create_session(&id, &SessionConfig::default())
            .await
            .unwrap();
        let copied: Vec<_> = original
            .iter()
            .cloned()
            .map(|mut record| {
                if let EventKind::Extension(data) = &record.event.kind
                    && data.domain() == "context-usage"
                {
                    let mut event = record.event.as_ref().clone();
                    event.kind = if schema == Some("context-usage/v4") {
                        let mut saved = data.data().clone();
                        if data.key() == "context-usage/recovered" {
                            saved["schema"] = "context-usage/v4".into();
                        }
                        EventKind::Extension(
                            ExtensionEvent::new(data.domain(), data.name(), saved).unwrap(),
                        )
                    } else {
                        EventKind::Extension(
                            ExtensionEvent::new("fixture", "omitted-cache", json!({})).unwrap(),
                        )
                    };
                    record.event = Arc::new(event);
                }
                record
            })
            .collect();
        store
            .append(key.clone(), copied, Box::new(|result| result.unwrap()))
            .unwrap();
        if let Some(schema) = schema {
            let timeline =
                Timeline::restore(Ctx::root().unwrap(), key.clone(), vec![], store.clone())
                    .await
                    .unwrap();
            let mut saved = latest.clone();
            saved["schema"] = schema.into();
            saved["source_through"] = timeline.sequence().into();
            if schema != "context-usage/v4" {
                let mut entries = entries.clone();
                let mut previous = previous.clone();
                if schema != "context-usage/v3" {
                    for summary in entries
                        .iter_mut()
                        .map(|entry| &mut entry["summary"])
                        .chain(std::iter::once(&mut previous))
                    {
                        summary.as_object_mut().unwrap().remove("growth");
                        summary.as_object_mut().unwrap().remove("last_entry");
                    }
                }
                let node = timeline
                    .append(
                        EventKind::Extension(
                            ExtensionEvent::new(
                                "context-usage",
                                "node",
                                json!({"kind":"leaf","entries":entries}),
                            )
                            .unwrap(),
                        )
                        .into(),
                    )
                    .unwrap();
                saved["state"] = json!({"branches":latest["branches"],"initialized":latest["initialized"],
                    "root":{"id":node.id,"first":entries[0]["at"],"last":entries.last().unwrap()["at"],
                    "height":0,"summary":previous}});
                for field in ["epoch", "branches", "initialized", "current"] {
                    saved.as_object_mut().unwrap().remove(field);
                }
            }
            timeline
                .append(
                    EventKind::Extension(
                        ExtensionEvent::new("context-usage", "recovered", saved).unwrap(),
                    )
                    .into(),
                )
                .unwrap();
            dim_agent_runtime::core::close_timeline(&timeline)
                .await
                .unwrap();
        }
        counter.messages.store(0, Ordering::SeqCst);
        let before = store.head(&key).await.unwrap();
        let mut profile = builder_with_usage_resolver(counter.clone());
        profile.timeline_store(store.clone());
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent.restore_session(id.clone()).await.unwrap();
        let value = session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            value.estimated_input_tokens, expected.estimated_input_tokens,
            "{schema:?}"
        );
        assert_eq!(
            counter.messages.load(Ordering::SeqCst),
            if schema.is_none() {
                initial_history.len()
            } else {
                0
            },
            "{schema:?}"
        );
        let added = store
            .memory
            .read(&key, before + 1..=store.head(&key).await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            added
                .iter()
                .filter(|record| matches!(&record.event.kind,
            EventKind::Extension(data) if data.key()=="context-usage/recovered"))
                .count(),
            usize::from(schema.is_none()),
            "only a missing aggregate writes an initial result: {schema:?}"
        );
        agent.close().await.unwrap();
        counter.messages.store(0, Ordering::SeqCst);
        store.context_pages.lock().unwrap().clear();
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent.restore_session(id.clone()).await.unwrap();
        let value = session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            value.estimated_input_tokens,
            expected.estimated_input_tokens
        );
        assert_eq!(
            counter.messages.load(Ordering::SeqCst),
            0,
            "reload recounted messages: {schema:?}"
        );
        assert!(
            store.context_pages.lock().unwrap().is_empty(),
            "reload projected history: {schema:?}"
        );
        session
            .submit("new input".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let value = session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert!(value.estimated_input_tokens > expected.estimated_input_tokens);
        agent.close().await.unwrap();
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent.restore_session(id.clone()).await.unwrap();
        let reloaded = session
            .service::<ContextPreparation>("main")
            .unwrap()
            .usage(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            reloaded.estimated_input_tokens, value.estimated_input_tokens,
            "v5 after new input: {schema:?}"
        );
        let cuts = chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
            .skip(1)
            .map(|event| event.id.clone())
            .collect::<Vec<_>>();
        agent.close().await.unwrap();
        for cut in cuts {
            let timeline =
                Timeline::restore(Ctx::root().unwrap(), key.clone(), vec![], store.clone())
                    .await
                    .unwrap();
            timeline.append(EventKind::ContextCompaction(ContextCompactionData {
                turn_id: None, request_id: None, command: None, start: None,
                through: timeline.sequence(),
                outcome: ContextCompactionResult::Completed {
                    retain_from: cut, replacement: vec![summary_message("replacement")],
                    strategy: "fixture".into(),
                    diagnostics: serde_json::from_value(json!({
                        "reason":"auto","trigger":"threshold","estimatedInputTokens":1,
                        "entriesReplaced":1,"modelCallCount":1,"durationMs":1,"strategy":{"kind":"fixture"}
                    })).unwrap(),
                    context_baseline: vec![], replacement_parts: None, pinned_inputs: None, digest_entries: None,
                },
            }).into()).unwrap();
            dim_agent_runtime::core::close_timeline(&timeline)
                .await
                .unwrap();
            let agent = MinimalHost::start(&profile).await.unwrap();
            let session = agent.restore_session(id.clone()).await.unwrap();
            let usage = session
                .service::<ContextPreparation>("main")
                .unwrap()
                .usage(CancellationToken::new())
                .await
                .unwrap();
            let context = HistoryRenderer::default()
                .context_in(&session, "main", usage.through)
                .await
                .unwrap();
            let messages = dim_agent_capabilities::history::context_messages(
                &context,
                &project_context_history(&context).unwrap(),
            );
            assert_eq!(
                usage.estimated_input_tokens,
                fixed
                    + messages
                        .iter()
                        .map(|message| serde_json::to_vec(message).unwrap().len() as f64)
                        .sum::<f64>(),
                "compaction after upgrade must match the actual context: {schema:?}"
            );
            agent.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn child_tool_results_do_not_change_usage_live_after_reload_or_compaction() {
    use dim_agent_capabilities::{context_preparation::ContextPreparation, prompt::Prompt};
    let store = Arc::new(MemoryTimelineStore::default());
    let counter = Arc::new(ObservedCounter::default());
    let mut profile = builder_with_usage_resolver(counter.clone());
    profile.timeline_store(store.clone());
    let cell = Arc::new(std::sync::OnceLock::new());
    let capture = cell.clone();
    profile.require_feature(Feature::new("test.raw-timeline").branch(
        "test.raw-timeline",
        move |names| {
            Ok(Box::new(super::manual_compaction::RawTimeline::new(
                names,
                capture.clone(),
            )?))
        },
    ));
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .service::<Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(ModelSelection {
                provider_id: "test".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let preparation = session.service::<ContextPreparation>("main").unwrap();
    let before = preparation.usage(CancellationToken::new()).await.unwrap();
    let counts = counter.messages.load(Ordering::SeqCst);
    let timeline = cell.get().unwrap();
    let call = timeline
        .append(
            EventDraft::new(EventKind::ToolCall(ToolCallData {
                turn_id: "child-turn".into(),
                request_id: "child-request".into(),
                calls: vec![ToolCall {
                    id: "child".into(),
                    name: "echo".into(),
                    input: JsonObject::new(),
                }],
            }))
            .with_parent_call_id("outer"),
        )
        .unwrap();
    let mut result = ToolResult::text("hidden child body".repeat(4096));
    result.context_messages = Some(vec![Message::text(Role::User, "hidden child context")]);
    timeline
        .append(
            EventDraft::new(EventKind::ToolResult(ToolResultData {
                turn_id: "child-turn".into(),
                request_id: "child-request".into(),
                call_event_id: Some(call.id.clone()),
                results: vec![ToolCallResult {
                    call_id: "child".into(),
                    result,
                }],
            }))
            .with_parent_call_id("outer"),
        )
        .unwrap();
    let live = preparation.usage(CancellationToken::new()).await.unwrap();
    assert_eq!(live.estimated_input_tokens, before.estimated_input_tokens);
    assert_eq!(
        counter.messages.load(Ordering::SeqCst),
        counts,
        "child content and contextMessages are never counted"
    );
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent.restore_session(id.clone()).await.unwrap();
    let restored = session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        restored.estimated_input_tokens,
        before.estimated_input_tokens
    );
    assert_eq!(
        counter.messages.load(Ordering::SeqCst),
        counts,
        "reload reuses the correct saved total"
    );
    session
        .submit("second".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    let cut = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .nth(1)
        .unwrap()
        .id
        .clone();
    agent.close().await.unwrap();
    let timeline = Timeline::restore(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: id.clone(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    )
    .await
    .unwrap();
    timeline.append(EventKind::ContextCompaction(ContextCompactionData {
        turn_id: None, request_id: None, command: None, start: None, through: timeline.sequence(),
        outcome: ContextCompactionResult::Completed {
            retain_from: cut, replacement: vec![summary_message("short")], strategy: "fixture".into(),
            diagnostics: serde_json::from_value(serde_json::json!({
                "reason":"auto","trigger":"threshold","estimatedInputTokens":1,
                "entriesReplaced":1,"modelCallCount":1,"durationMs":1,"strategy":{"kind":"fixture"}
            })).unwrap(),
            context_baseline: vec![], replacement_parts: None, pinned_inputs: None, digest_entries: None,
        },
    }).into()).unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent.restore_session(id).await.unwrap();
    let usage = session
        .service::<ContextPreparation>("main")
        .unwrap()
        .usage(CancellationToken::new())
        .await
        .unwrap();
    let context = HistoryRenderer::default()
        .context_in(&session, "main", usage.through)
        .await
        .unwrap();
    let messages = dim_agent_capabilities::history::context_messages(
        &context,
        &project_context_history(&context).unwrap(),
    );
    assert_eq!(
        usage.estimated_input_tokens,
        counter.fixed_value.lock().unwrap().0 as f64
            + messages
                .iter()
                .map(|message| serde_json::to_vec(message).unwrap().len() as f64)
                .sum::<f64>(),
        "compaction subtracts only actual context contributions"
    );
    agent.close().await.unwrap();
}

/// A restore asks for consumptions only for the inputs it adopts: none when the
/// entry it adopts comes from a change that names no input, and exactly one
/// filtered to those ids when it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_asks_for_consumptions_only_for_the_inputs_it_adopts() {
    use dim_agent_capabilities::prompt::Prompt;

    // A System entry is delivered by the prefix's configuration, so its change
    // names no input; a Context entry is delivered by a block, so its change
    // names one.
    for delivery in [PromptDelivery::System, PromptDelivery::Context] {
        let store = Arc::new(CountContextReads::default());
        let mut profile = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        profile.timeline_store(store.clone());
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let id = session.id().to_owned();
        let scope = PromptScope::Branch {
            timeline: "main".into(),
        };
        session
            .service::<Prompt>("main")
            .unwrap()
            .configure_loaded(
                Some(ModelSelection {
                    provider_id: "fixture".into(),
                    model_id: "counter".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                vec![(
                    PromptSourceTarget {
                        source: "rules".into(),
                        scope: scope.clone(),
                    },
                    Some(PromptEntry {
                        source: "rules".into(),
                        version: "1".into(),
                        body: "inherited".into(),
                        scope,
                        delivery,
                        order: 0,
                    }),
                )],
            )
            .await
            .unwrap();
        let change = chain_events(&session, "main", ..)
            .await
            .unwrap()
            .into_iter()
            .find_map(|event| match &event.kind {
                EventKind::PromptChanged(data) => Some(data.clone()),
                _ => None,
            })
            .expect("main wrote its change");
        assert_eq!(
            change.inputs.is_empty(),
            delivery == PromptDelivery::System,
            "only a block-delivered change names an input"
        );
        // A child's state is restored from the chain, which is where it adopts
        // the change.
        let child = session
            .branch(
                "main".into(),
                session.sequence("main").unwrap(),
                BranchOptions {
                    prompt: "child".into(),
                    source: BranchSource::Host {
                        operation_id: "count".into(),
                    },
                },
            )
            .await
            .unwrap();
        agent.close().await.unwrap();
        drop(session);

        let mut profile = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        profile.timeline_store(store.clone());
        let agent = MinimalHost::start(&profile).await.unwrap();
        store.property_queries.lock().unwrap().clear();
        let session = agent.restore_session(id.clone()).await.unwrap();
        // The checkpoint rows the walk read: one per query whose kind is the
        // recovery key. Read them before anything else queries the store.
        let rows = store
            .property_queries
            .lock()
            .unwrap()
            .iter()
            .filter(|query| {
                query.filter.as_ref().is_some_and(|filter| {
                    let mut filters = vec![filter];
                    let mut kinds = Vec::new();
                    while let Some(filter) = filters.pop() {
                        match filter {
                            QueryFilter::In {
                                value: QueryOperand::Field { path },
                                values,
                            } if path == "kind" => kinds.extend(
                                values
                                    .iter()
                                    .filter_map(|value| value.as_str().map(str::to_owned)),
                            ),
                            QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                                filters.extend(conditions)
                            }
                            _ => {}
                        }
                    }
                    kinds == ["prompt-state/recovered"]
                })
            })
            .count();
        let asked: Vec<Vec<String>> = store
            .property_queries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|query| {
                let mut filters = vec![query.filter.as_ref()?];
                let (mut kinds, mut ids) = (Vec::new(), Vec::new());
                while let Some(filter) = filters.pop() {
                    match filter {
                        QueryFilter::In {
                            value: QueryOperand::Field { path },
                            values,
                        } if path == "kind" => kinds.extend(
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_owned)),
                        ),
                        QueryFilter::In {
                            value: QueryOperand::Field { path },
                            values,
                        } if path == "data.input_id" => ids.extend(
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_owned)),
                        ),
                        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                            filters.extend(conditions)
                        }
                        _ => {}
                    }
                }
                (kinds == ["input/consumed"]).then_some(ids)
            })
            .collect();
        // The line's own scope has no checkpoint, so the chain's newest row is
        // read. When the change was never received the row is read again
        // positioned before it, and the read after that settles; when the
        // prefix delivered it the newest row is taken outright.
        assert_eq!(
            rows,
            if change.inputs.is_empty() { 3 } else { 2 },
            "checkpoint rows read for a change with inputs {:?}",
            change.inputs
        );
        assert!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
                .unwrap()
                .is_some(),
            "the child has a prompt state"
        );
        // One consumption query, over exactly the union of the inputs the
        // inherited changes after the last request start name: not one per
        // candidate, and nothing wider than those ids. A System change names no
        // input, so it contributes nothing and, when it is the only change, no
        // query is issued at all.
        if change.inputs.is_empty() {
            assert!(
                asked.is_empty(),
                "a System entry asks about no input: {asked:?}"
            );
        } else {
            let mut expected: Vec<String> = change.inputs.clone();
            expected.sort();
            for ids in &asked {
                let mut sorted = ids.clone();
                sorted.sort();
                assert_eq!(
                    sorted, expected,
                    "the query asks for exactly the inherited inputs: {asked:?}"
                );
            }
        }
        agent.close().await.unwrap();
    }
}

/// A change that names two delivery inputs applies only once both were consumed
/// in the inherited prefix: hiding either one's consumption leaves it
/// undelivered, so neither "only the first gates" nor "only the last" survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_naming_two_inputs_waits_for_both() {
    use dim_agent_capabilities::prompt::Prompt;

    // `hidden` names the input whose consumption the store withholds from the
    // child's read: none, the first, the second.
    for hidden in [None, Some(0usize), Some(1usize)] {
        let store = Arc::new(CountContextReads::default());
        let mut profile = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        profile.timeline_store(store.clone());
        let agent = MinimalHost::start(&profile).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let id = session.id().to_owned();
        let scope = PromptScope::Branch {
            timeline: "main".into(),
        };
        let entry = |source: &str, body: &str| PromptEntry {
            source: source.into(),
            version: "1".into(),
            body: body.into(),
            scope: scope.clone(),
            delivery: PromptDelivery::Context,
            order: 0,
        };
        // Two Context-delivered sources in one adoption: one change naming the
        // two blocks that deliver them.
        session
            .service::<Prompt>("main")
            .unwrap()
            .configure_loaded(
                Some(ModelSelection {
                    provider_id: "fixture".into(),
                    model_id: "counter".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                vec![
                    (
                        PromptSourceTarget {
                            source: "rules-a".into(),
                            scope: scope.clone(),
                        },
                        Some(entry("rules-a", "A")),
                    ),
                    (
                        PromptSourceTarget {
                            source: "rules-b".into(),
                            scope: scope.clone(),
                        },
                        Some(entry("rules-b", "B")),
                    ),
                ],
            )
            .await
            .unwrap();
        let change = chain_events(&session, "main", ..)
            .await
            .unwrap()
            .into_iter()
            .find_map(|event| match &event.kind {
                EventKind::PromptChanged(data) => Some(data.clone()),
                _ => None,
            })
            .expect("main wrote its change");
        assert_eq!(
            change.inputs.len(),
            2,
            "one change names both delivery inputs: {:?}",
            change.inputs
        );
        // One turn consumes both blocks, so both consumptions are in the
        // history the child inherits.
        session
            .submit("first".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let main = chain_events(&session, "main", ..).await.unwrap();
        for input in &change.inputs {
            assert!(
                main.iter().any(|event| matches!(
                    &event.kind,
                    EventKind::InputConsumed(data) if &data.input_id == input
                )),
                "main consumed {input}"
            );
        }
        let cut = session.sequence("main").unwrap();
        // The child is cut after the consumption and adopts the change from the
        // chain, which is where the gate runs.
        let child = session
            .branch(
                "main".into(),
                cut,
                BranchOptions {
                    prompt: "child".into(),
                    source: BranchSource::Host {
                        operation_id: "two".into(),
                    },
                },
            )
            .await
            .unwrap();
        agent.close().await.unwrap();
        drop(session);

        let mut profile = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        profile.timeline_store(store.clone());
        let agent = MinimalHost::start(&profile).await.unwrap();
        if let Some(at) = hidden {
            *store.hidden_consumptions.lock().unwrap() = vec![change.inputs[at].clone()];
        }
        let session = agent.restore_session(id.clone()).await.unwrap();
        let state = session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
            .unwrap()
            .unwrap();
        let sources: Vec<&str> = state
            .entries
            .keys()
            .map(|(source, _)| source.as_str())
            .collect();
        assert_eq!(
            sources.contains(&"rules-a") && sources.contains(&"rules-b"),
            hidden.is_none(),
            "with {hidden:?} consumed the change is {}; state holds {sources:?}",
            if hidden.is_none() {
                "delivered"
            } else {
                "undelivered"
            }
        );
        agent.close().await.unwrap();
    }
}

/// A restore whose newest Prompt checkpoint is a `prompt-state/v1` row reads its
/// boundary from `source_through`, as that schema wrote it, rather than failing
/// on the `changed_through` field v1 never had.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_reads_a_v1_checkpoints_boundary_from_source_through() {
    use dim_agent_capabilities::prompt::Prompt;

    let store = Arc::new(CountContextReads::default());
    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    // One adoption writes a change, so the chain has a boundary to name.
    let scope = PromptScope::Branch {
        timeline: "main".into(),
    };
    session
        .service::<Prompt>("main")
        .unwrap()
        .configure_loaded(
            None,
            vec![(
                PromptSourceTarget {
                    source: "rules".into(),
                    scope: scope.clone(),
                },
                Some(PromptEntry {
                    source: "rules".into(),
                    version: "1".into(),
                    body: "kept".into(),
                    scope: scope.clone(),
                    delivery: PromptDelivery::Context,
                    order: 0,
                }),
            )],
        )
        .await
        .unwrap();
    agent.close().await.unwrap();
    drop(session);

    // The checkpoint this branch wrote is `prompt-state/v2`; rewrite it as the
    // shape `prompt-state/v1` wrote - `source_through`, no `changed_through` -
    // so restore has to derive the boundary the way the v1 arm does.
    let key = dim_agent_runtime::TimelineKey {
        session_id: id.clone(),
        branch_id: "main".into(),
    };
    let head = store.head(&key).await.unwrap();
    let records = store.read(&key, 1..=head).await.unwrap();
    let mut drafts = Vec::new();
    for record in &records {
        let EventKind::Extension(data) = &record.event.kind else {
            continue;
        };
        if data.key() != "prompt-state/recovered" {
            continue;
        }
        let mut saved = data.data().clone();
        assert_eq!(
            saved["schema"], "prompt-state/v2",
            "the checkpoint v2 wrote"
        );
        saved["schema"] = "prompt-state/v1".into();
        // v1 named its boundary by the sequence it had read through, in the
        // line's own numbering; the v2 field is a position object instead.
        saved["source_through"] = head.into();
        saved.as_object_mut().unwrap().remove("changed_through");
        drafts.push(EventDraft::new(EventKind::Extension(
            ExtensionEvent::new("prompt-state", "recovered", saved).unwrap(),
        )));
    }
    assert_eq!(drafts.len(), 1, "one v1 checkpoint to seed");
    let timeline = dim_agent_runtime::Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key.clone(),
        Vec::new(),
        store.clone(),
    )
    .await
    .unwrap();
    timeline
        .append_batch_registered(drafts, |_| Ok(()))
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();

    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .restore_session(id)
        .await
        .expect("a v1 checkpoint restores");
    let state = session
        .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
        .unwrap()
        .unwrap();
    assert!(
        state.entries.values().any(|entry| entry.body == "kept"),
        "the entry the checkpoint carries survives the restore"
    );
    agent.close().await.unwrap();
}

/// A checkpoint's boundary being delivered does not mean everything it covers
/// was: a System-delivered change has no inputs, so it is delivered wherever it
/// is found, and a checkpoint written at it can still cover an earlier removal
/// the line never received. The line must start from a row positioned before
/// that removal.
///
/// The bundled System sources (`dim/base`, the Session entry) do not change
/// while a session runs, so the sequence is seeded into the store rather than
/// driven through a flow. A plugin may declare a source delivered as System, and
/// such a source can change while main is idle; that is the case this pins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_starts_before_an_undelivered_removal_under_a_later_system_change() {
    let store = Arc::new(CountContextReads::default());
    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    drop(session);

    let key = dim_agent_runtime::TimelineKey {
        session_id: id.clone(),
        branch_id: "main".into(),
    };
    let scope = PromptScope::Branch {
        timeline: "main".into(),
    };
    let timeline = dim_agent_runtime::Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key.clone(),
        Vec::new(),
        store.clone(),
    )
    .await
    .unwrap();
    // (1) main adopts the rule A, delivered by the block `i1`.
    let adopted = timeline
        .append_batch_registered(
            vec![
                EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                    "first",
                    Some(InputMode::Append),
                )))
                .with_id("i1"),
                EventDraft::new(EventKind::PromptChanged(PromptChangedData {
                    timeline: "main".into(),
                    revision: "r1".into(),
                    previous: None,
                    reason: PromptChangeReason::Refresh,
                    changes: vec![PromptChange::Set {
                        entry: PromptEntry {
                            source: "rules".into(),
                            version: "1".into(),
                            body: "kept".into(),
                            scope: scope.clone(),
                            delivery: PromptDelivery::Context,
                            order: 0,
                        },
                    }],
                    inputs: vec!["i1".into()],
                })),
            ],
            |_| Ok(()),
        )
        .unwrap();
    let adopted = adopted.last().expect("the adoption").sequence;
    // A request consumes `i1`, so the adoption was delivered on this prefix.
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "t1".into(),
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: "i1".into(),
            })
            .into(),
        )
        .unwrap();
    // A checkpoint at the adoption: the rule is live and delivered.
    let first = timeline
        .append(
            EventKind::Extension(
                ExtensionEvent::new(
                    "prompt-state",
                    "recovered",
                    serde_json::to_value(RecoveredSeed {
                        schema: "prompt-state/v2".into(),
                        changed_through: Some(dim_agent_types::TimelinePosition {
                            branch_id: "main".into(),
                            sequence: adopted,
                        }),
                        state: RecoverySeed {
                            entries: vec![EntryReferenceSeed {
                                source: "rules".into(),
                                scope: scope.clone(),
                                event_id: store
                                    .read(&key, adopted..=adopted)
                                    .await
                                    .unwrap()
                                    .first()
                                    .map(|record| record.event.id.clone())
                                    .expect("the adoption event"),
                                element_index: 0,
                            }],
                            revision: Some(PromptReference {
                                timeline: "main".into(),
                                revision: "r1".into(),
                            }),
                            system_revision: None,
                        },
                    })
                    .unwrap(),
                )
                .unwrap(),
            )
            .into(),
        )
        .unwrap()
        .sequence;
    // (2) main revokes A, and the retirement input `i2` stays pending.
    timeline
        .append_batch_registered(
            vec![
                EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                    "retire",
                    Some(InputMode::Append),
                )))
                .with_id("i2"),
                EventDraft::new(EventKind::PromptChanged(PromptChangedData {
                    timeline: "main".into(),
                    revision: "r2".into(),
                    previous: None,
                    reason: PromptChangeReason::Refresh,
                    changes: vec![PromptChange::Revoke {
                        source: "rules".into(),
                        scope: scope.clone(),
                    }],
                    inputs: vec!["i2".into()],
                })),
            ],
            |_| Ok(()),
        )
        .unwrap();
    // (3) main writes a System-only change B: no inputs, a later revision.
    let system = timeline
        .append(
            EventKind::PromptChanged(PromptChangedData {
                timeline: "main".into(),
                revision: "r3".into(),
                previous: None,
                reason: PromptChangeReason::Refresh,
                changes: vec![PromptChange::Set {
                    entry: PromptEntry {
                        source: "dim/base".into(),
                        version: "2".into(),
                        body: "system text".into(),
                        scope: scope.clone(),
                        delivery: PromptDelivery::System,
                        order: 0,
                    },
                }],
                inputs: Vec::new(),
            })
            .into(),
        )
        .unwrap()
        .sequence;
    // (4) main writes its checkpoint at B, with A already gone.
    timeline
        .append(
            EventKind::Extension(
                ExtensionEvent::new(
                    "prompt-state",
                    "recovered",
                    serde_json::to_value(RecoveredSeed {
                        schema: "prompt-state/v2".into(),
                        changed_through: Some(dim_agent_types::TimelinePosition {
                            branch_id: "main".into(),
                            sequence: system,
                        }),
                        state: RecoverySeed {
                            entries: Vec::new(),
                            revision: Some(PromptReference {
                                timeline: "main".into(),
                                revision: "r3".into(),
                            }),
                            system_revision: Some(PromptReference {
                                timeline: "main".into(),
                                revision: "r3".into(),
                            }),
                        },
                    })
                    .unwrap(),
                )
                .unwrap(),
            )
            .into(),
        )
        .unwrap();
    let head = timeline.committed();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();

    // (5) the child is cut at main's head and restores.
    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent.restore_session(id).await.unwrap();
    let child = session
        .branch(
            "main".into(),
            head,
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "p2".into(),
                },
            },
        )
        .await
        .unwrap();
    let state = session
        .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
        .unwrap()
        .unwrap();
    // The child's prefix never delivered the removal, so the rule is still live
    // there: its state carries A, and it does not carry B's checkpoint view.
    assert!(
        state.entries.values().any(|entry| entry.body == "kept"),
        "the rule the removal never delivered is still live: {:?}",
        state
            .entries
            .values()
            .map(|entry| (&entry.source, &entry.body))
            .collect::<Vec<_>>()
    );
    // The gated suffix replay drops the removal it never received and applies
    // what it did, including the System change's entry, which a configuration
    // delivers wherever it is found.
    assert!(
        state
            .entries
            .values()
            .any(|entry| entry.body == "system text"),
        "the System entry after the removal applies where it is found"
    );
    let _ = first;
    agent.close().await.unwrap();
}

/// The shape `prompt-state/v2` writes, restated here so the test seeds the same
/// row a real checkpoint does.
#[derive(serde::Serialize)]
struct RecoveredSeed {
    schema: String,
    changed_through: Option<dim_agent_types::TimelinePosition>,
    state: RecoverySeed,
}

#[derive(serde::Serialize)]
struct RecoverySeed {
    entries: Vec<EntryReferenceSeed>,
    revision: Option<PromptReference>,
    system_revision: Option<PromptReference>,
}

#[derive(serde::Serialize)]
struct EntryReferenceSeed {
    source: String,
    scope: PromptScope,
    event_id: String,
    element_index: u64,
}

/// A long turn keeps every refresh made since its `turn/start`, including ones
/// mid-turn requests already consumed, because no `turn/start` is written per
/// model request; an idle parent can do the same with plain refreshes. The
/// suffix after that start can therefore span many pages of changes, and each
/// change can name more than one input. Every consumption query Prompt issues
/// stays bounded, whichever page it is judging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_inherited_suffix_keeps_each_consumption_query_bounded() {
    // Enough refreshes to span more than one page of `prompt/changed` rows.
    const REFRESHES: u32 = 200;

    let store = Arc::new(CountContextReads::default());
    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    drop(session);

    let key = dim_agent_runtime::TimelineKey {
        session_id: id.clone(),
        branch_id: "main".into(),
    };
    let scope = PromptScope::Branch {
        timeline: "main".into(),
    };
    let timeline = dim_agent_runtime::Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key.clone(),
        Vec::new(),
        store.clone(),
    )
    .await
    .unwrap();
    // S: a request start whose turn stays open, as a long turn's does.
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "t1".into(),
            })
            .into(),
        )
        .unwrap();
    // After S, refreshes that were consumed, standing for mid-turn requests.
    // Each names two inputs and both are consumed.
    for n in 0..REFRESHES {
        let first = format!("a{n}");
        let second = format!("b{n}");
        timeline
            .append_batch_registered(
                vec![
                    EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                        "turn",
                        Some(InputMode::Append),
                    )))
                    .with_id(first.clone()),
                    EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                        "turn",
                        Some(InputMode::Append),
                    )))
                    .with_id(second.clone()),
                    EventDraft::new(EventKind::PromptChanged(PromptChangedData {
                        timeline: "main".into(),
                        revision: format!("r{n}"),
                        previous: None,
                        reason: PromptChangeReason::Refresh,
                        changes: vec![PromptChange::Set {
                            entry: PromptEntry {
                                source: "rules".into(),
                                version: format!("{n}"),
                                body: format!("value {n}"),
                                scope: scope.clone(),
                                delivery: PromptDelivery::Context,
                                order: 0,
                            },
                        }],
                        inputs: vec![first.clone(), second.clone()],
                    })),
                    EventDraft::new(EventKind::InputConsumed(InputConsumedData {
                        input_id: first,
                    })),
                    EventDraft::new(EventKind::InputConsumed(InputConsumedData {
                        input_id: second,
                    })),
                ],
                |_| Ok(()),
            )
            .unwrap();
    }
    // The last refresh names two inputs that are never consumed: it is E.
    timeline
        .append_batch_registered(
            vec![
                EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                    "turn",
                    Some(InputMode::Append),
                )))
                .with_id("pending-a"),
                EventDraft::new(EventKind::InputMessage(InputMessageData::text(
                    "turn",
                    Some(InputMode::Append),
                )))
                .with_id("pending-b"),
                EventDraft::new(EventKind::PromptChanged(PromptChangedData {
                    timeline: "main".into(),
                    revision: "pending".into(),
                    previous: None,
                    reason: PromptChangeReason::Refresh,
                    changes: vec![PromptChange::Set {
                        entry: PromptEntry {
                            source: "rules".into(),
                            version: "pending".into(),
                            body: "value pending".into(),
                            scope: scope.clone(),
                            delivery: PromptDelivery::Context,
                            order: 0,
                        },
                    }],
                    inputs: vec!["pending-a".into(), "pending-b".into()],
                })),
            ],
            |_| Ok(()),
        )
        .unwrap();
    let head = timeline.committed();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let before = store.property_queries.lock().unwrap().len();

    let mut profile = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    profile.timeline_store(store.clone());
    let agent = MinimalHost::start(&profile).await.unwrap();
    let session = agent
        .restore_session(id)
        .await
        .expect("the restore succeeds");
    let child = session
        .branch(
            "main".into(),
            head,
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "long".into(),
                },
            },
        )
        .await
        .unwrap();
    let state = session
        .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
        .unwrap()
        .unwrap();
    // The gate found E past the first page: the entries are the ones the
    // refreshes up to 199 wrote, and the pending one is not among them.
    let bodies: Vec<&str> = state
        .entries
        .values()
        .map(|entry| entry.body.as_str())
        .collect();
    assert_eq!(
        bodies,
        vec![format!("value {}", REFRESHES - 1).as_str()],
        "the state carries the last delivered refresh, not the pending one"
    );
    // Every consumption query Prompt issued is bounded. Collected first,
    // because the guard must not be held across the close below.
    let widths: Vec<usize> = {
        let property_queries = store.property_queries.lock().unwrap();
        let mut widths = Vec::new();
        for query in property_queries[before..].iter() {
            let mut filters: Vec<_> = query.filter.iter().collect();
            while let Some(filter) = filters.pop() {
                match filter {
                    QueryFilter::In {
                        value: QueryOperand::Field { path },
                        values,
                    } if path == "data.input_id" => widths.push(values.len()),
                    QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                        filters.extend(conditions)
                    }
                    _ => {}
                }
            }
        }
        widths
    };
    assert!(!widths.is_empty(), "a consumption query was issued");
    for width in widths {
        assert!(
            width <= 128,
            "a consumption query names at most 128 ids: {width}"
        );
    }
    agent.close().await.unwrap();
}
