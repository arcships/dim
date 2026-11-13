//! The no-Loop part harness: the real Dispatcher, Timeline,
//! TimelineTree, SessionMethods, ToolRegistry, PromptGuide, storage and
//! Settings, with only the parts Goal needs. The facts the Loop, InputQueue
//! and TimelineTree would write are the test's.
#![allow(dead_code)]
use dim_agent_capabilities::host::ToolExecutionContext;
use dim_agent_capabilities::methods::RpcError;
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_capabilities::{
    defaults,
    file_storage::FileStoragePlugin,
    methods::SessionMethods,
    prompt_guide::{GuideEntries, PromptGuidePlugin},
    timeline_tree::{TimelineTreePlugin, TreeTimelinePlugin},
    tool_registry::{ToolRegistry, ToolRegistryPlugin},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, BoxFuture, BranchOptions, Dispatcher, Feature,
    LoopSnapshot, LoopState, MemoryTimelineStore, Names, PublishCallback, Session, SessionConfig,
    StoredBranch, StoredEvent, Timeline, TimelineIdentity, TimelineKey, TimelineQueryDirection,
    TimelineStore, TimelineTrace,
};
use dim_agent_types::{
    AgentEvent, BranchSource, DeltaData, EventKind, InputBlock, InputMessageData, InputMode,
    StepData, ToolCall, ToolDefinition, ToolResult,
};
use dim_app_server::settings::{SettingsPlugin, SettingsService};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_loader::resolve::Build;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::RangeInclusive,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// The branch facts the test appends, and the services the assertions read.
#[derive(Default)]
pub struct Probe {
    timelines: Mutex<HashMap<String, Arc<Timeline>>>,
    commands: Mutex<HashMap<String, Arc<TreeCommands>>>,
    dispatchers: Mutex<HashMap<String, Arc<Dispatcher>>>,
    tools: Mutex<Option<Arc<ToolRegistry>>>,
    guide: Mutex<Option<Arc<GuideEntries>>>,
    store: Mutex<Option<Arc<dyn TimelineStore>>>,
}

impl Probe {
    pub fn timeline(&self, branch: &str) -> Arc<Timeline> {
        self.timelines
            .lock()
            .unwrap()
            .get(branch)
            .cloned()
            .unwrap_or_else(|| panic!("branch {branch} has no Timeline"))
    }
    pub fn has(&self, branch: &str) -> bool {
        self.timelines.lock().unwrap().contains_key(branch)
    }
    pub fn commands(&self, branch: &str) -> Arc<TreeCommands> {
        self.commands
            .lock()
            .unwrap()
            .get(branch)
            .cloned()
            .unwrap_or_else(|| panic!("branch {branch} has no TreeCommands"))
    }
    pub fn dispatcher(&self, branch: &str) -> Arc<Dispatcher> {
        self.dispatchers
            .lock()
            .unwrap()
            .get(branch)
            .cloned()
            .unwrap_or_else(|| panic!("branch {branch} has no Dispatcher"))
    }
    pub fn tools(&self) -> Arc<ToolRegistry> {
        self.tools.lock().unwrap().clone().expect("ToolRegistry")
    }
    pub fn guide(&self) -> Arc<GuideEntries> {
        self.guide.lock().unwrap().clone().expect("GuideEntries")
    }
    pub fn store(&self) -> Arc<dyn TimelineStore> {
        self.store.lock().unwrap().clone().expect("TimelineStore")
    }
}

struct ProbeSession {
    probe: Arc<Probe>,
    tools: Key<ToolRegistry>,
    guide: Key<GuideEntries>,
    store: Key<dyn TimelineStore>,
}
impl ProbeSession {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            tools: names.key()?,
            guide: names.key()?,
            store: names.key()?,
        })
    }
}
impl DynPlugin for ProbeSession {
    fn name(&self) -> &str {
        "GoalProbeSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase(), self.guide.erase(), self.store.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            let guide = ctx.require_at(&self.guide)?;
            let store = ctx.require_at(&self.store)?;
            *self.probe.tools.lock().unwrap() = Some(tools);
            *self.probe.guide.lock().unwrap() = Some(guide);
            *self.probe.store.lock().unwrap() = Some(store);
            Ok(Effect::Done)
        })
    }
}

struct ProbeBranch {
    probe: Arc<Probe>,
    timeline: Key<Timeline>,
    commands: Key<TreeCommands>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
}
impl ProbeBranch {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            timeline: names.key()?,
            commands: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for ProbeBranch {
    fn name(&self) -> &str {
        "GoalProbeBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.commands.erase(),
            self.dispatcher.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let commands = ctx.require_at(&self.commands)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let mut timelines = self.probe.timelines.lock().unwrap();
            timelines.insert(identity.branch_id.clone(), timeline);
            self.probe
                .commands
                .lock()
                .unwrap()
                .insert(identity.branch_id.clone(), commands);
            self.probe
                .dispatchers
                .lock()
                .unwrap()
                .insert(identity.branch_id.clone(), dispatcher);
            Ok(Effect::Done)
        })
    }
}

/// Every branch must provide the Loop's state, which Goal never reads. The
/// harness supplies an idle state instead of the AgentLoop, so no turn is
/// driven and the test owns every fact.
struct IdleLoop(tokio::sync::watch::Sender<LoopSnapshot>);
impl LoopState for IdleLoop {
    fn state(&self) -> LoopSnapshot {
        LoopSnapshot::default()
    }
    fn watch(&self) -> tokio::sync::watch::Receiver<LoopSnapshot> {
        self.0.subscribe()
    }
}

struct LoopStub {
    key: Key<dyn LoopState>,
    watch: tokio::sync::watch::Sender<LoopSnapshot>,
}
impl LoopStub {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            key: names.key()?,
            watch: tokio::sync::watch::channel(LoopSnapshot::default()).0,
        })
    }
}
impl DynPlugin for LoopStub {
    fn name(&self) -> &str {
        "GoalLoopStub"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.provide_at(
                self.key.clone(),
                Arc::new(IdleLoop(self.watch.clone())) as Arc<dyn LoopState>,
            )?;
            Ok(Effect::Done)
        })
    }
}

/// A Session record being filled with facts, branch by branch, before a
/// restore loads it.
pub struct Seed {
    root: Ctx,
    store: Arc<dyn TimelineStore>,
    session: String,
    timelines: HashMap<String, Arc<Timeline>>,
}

impl Seed {
    /// Commits `kind` on `branch` and returns the fact, so a case can use its
    /// id or sequence in a later fact of the same history.
    pub fn append(&mut self, branch: &str, kind: EventKind) -> Arc<AgentEvent> {
        let timeline = self.timelines.entry(branch.to_owned()).or_insert_with(|| {
            Arc::new(Timeline::new(
                self.root.clone(),
                TimelineKey {
                    session_id: self.session.clone(),
                    branch_id: branch.to_owned(),
                },
                vec![],
                self.store.clone(),
            ))
        });
        timeline
            .append(kind.into())
            .unwrap_or_else(|error| panic!("seeding {branch}: {error}"))
    }

    /// Commits `kinds` as one atomic batch and returns the facts, so a case
    /// can seed a batch Goal writes together (R5).
    pub fn append_batch(&mut self, branch: &str, kinds: Vec<EventKind>) -> Vec<Arc<AgentEvent>> {
        let timeline = self.timelines.entry(branch.to_owned()).or_insert_with(|| {
            Arc::new(Timeline::new(
                self.root.clone(),
                TimelineKey {
                    session_id: self.session.clone(),
                    branch_id: branch.to_owned(),
                },
                vec![],
                self.store.clone(),
            ))
        });
        timeline
            .append_batch_registered(kinds.into_iter().map(EventKind::into).collect(), |_| Ok(()))
            .unwrap_or_else(|error| panic!("seeding a batch on {branch}: {error}"))
    }

    /// Closes the seeding root; the facts stay in the store.
    pub async fn finish(self) {
        for timeline in self.timelines.values() {
            let through = timeline.committed();
            let mut published = timeline.watch();
            let state = published
                .wait_for(|(sequence, closed)| *sequence >= through || *closed)
                .await
                .expect("seed publication remains observable");
            assert!(
                state.0 >= through,
                "all seeded batches publish before restore"
            );
        }
        let _ = self.root.shutdown().await;
    }
}

/// The queried branch, projection and fixed bounds observed at the storage port.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    pub select: dim_agent_runtime::QuerySelect,
    pub branch: String,
    pub kinds: Vec<String>,
    pub direction: TimelineQueryDirection,
    /// The first sequence the window admits.
    pub from: u64,
    /// The last sequence the window admits.
    pub through: u64,
    pub limit: u64,
}

/// A store that records every [`TimelineStore::query`] it receives, and that
/// can refuse them for one branch while armed, returning the storage error a
/// real store returns for a failed read; every other call delegates to a
/// memory store.
#[derive(Default)]
pub struct RefusingStore {
    memory: MemoryTimelineStore,
    armed: AtomicBool,
    branch: Mutex<Option<String>>,
    refused: AtomicUsize,
    reads: Mutex<Vec<Read>>,
}

impl RefusingStore {
    /// Refuses queries on `branch` until [`Self::disarm`].
    pub fn arm(&self, branch: &str) {
        *self.branch.lock().unwrap() = Some(branch.to_owned());
        self.armed.store(true, Ordering::SeqCst);
    }

    pub fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }

    /// Queries refused since the store was created.
    pub fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }

    /// Every read the store saw, in the order it received them.
    pub fn reads(&self) -> Vec<Read> {
        self.reads.lock().unwrap().clone()
    }
}

fn query_kinds(filter: Option<&dim_agent_runtime::QueryFilter>) -> Vec<String> {
    use dim_agent_runtime::{QueryFilter, QueryOperand};
    match filter {
        Some(QueryFilter::Compare {
            left: QueryOperand::Field { path },
            comparison: dim_agent_runtime::QueryComparison::Eq,
            right: QueryOperand::Value { value },
        }) if path == "kind" => value.as_str().into_iter().map(str::to_owned).collect(),
        Some(QueryFilter::In {
            value: QueryOperand::Field { path },
            values,
        }) if path == "kind" => values
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect(),
        Some(QueryFilter::And { conditions }) | Some(QueryFilter::Or { conditions }) => conditions
            .iter()
            .flat_map(|condition| query_kinds(Some(condition)))
            .collect(),
        _ => vec![],
    }
}

impl TimelineStore for RefusingStore {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        let kinds = query_kinds(query.filter.as_ref());
        let direction = query
            .order_by
            .first()
            .map_or(TimelineQueryDirection::Ascending, |order| order.direction);
        self.reads.lock().unwrap().push(Read {
            select: query.select.clone(),
            branch: scope.key.branch_id.clone(),
            kinds: kinds.clone(),
            direction,
            from: scope.segments.first().map_or(1, |segment| segment.from),
            through: scope.through,
            limit: page.map_or(0, |page| page.limit.get()),
        });
        if self.armed.load(Ordering::SeqCst)
            && self.branch.lock().unwrap().as_deref() == Some(scope.key.branch_id.as_str())
            && kinds == ["input/message"]
            && direction == TimelineQueryDirection::Descending
        {
            self.refused.fetch_add(1, Ordering::SeqCst);
            return Box::pin(async {
                Err(AgentError::Storage("the inherited read was refused".into()))
            });
        }
        self.memory.query(scope, query, page)
    }

    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(session_id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(session_id)
    }
    fn release_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(session_id)
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Option<SessionConfig>>> {
        self.memory.session_config(session_id)
    }
    fn branches<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Vec<StoredBranch>>> {
        self.memory.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        self.memory.append(key, events, publish)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.batch_end(key, sequence)
    }

    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

/// A store that holds Goal's creation check — its one `branch/create` query —
/// until the case lets it go, so a case can commit a change and have the fold
/// consume it while that check waits; every other call delegates to a memory
/// store. It also holds one publication or one read at a time, so a case can
/// place a fact in a window a live system never shows it in.
#[derive(Default)]
pub struct GatedStore {
    memory: MemoryTimelineStore,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// A continuation input's publication, held until the case releases it.
    published: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    /// A state record's publication and every publication after it, held until
    /// the case releases them.
    state: Arc<Mutex<Option<StateHold>>>,
    /// A read whose one kind matches, held until the case releases it.
    read: Mutex<Option<ReadHold>>,
    fail_read: AtomicBool,
}

/// One held read: its kind selection, the one branch it must not be on, and
/// the case's ends of the gate.
struct ReadHold {
    kinds: Vec<String>,
    not_branch: Option<String>,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// One held publication: the state record naming `after` and everything
/// committed after it wait, in order, for the case's release (R2, R4).
struct StateHold {
    /// The state record's `after`, or `None` for the next state record.
    after: Option<u64>,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
    /// Set when the record's publication arrived: later ones queue behind it.
    triggered: bool,
    /// Set when the case released the gate: the queue drains and later
    /// publications run directly.
    released: bool,
    queue: VecDeque<Box<dyn FnOnce() + Send>>,
}

impl StateHold {
    /// Whether one of `events` is a state record naming the held `after`.
    fn matches(&self, events: &[StoredEvent]) -> bool {
        events.iter().any(|stored| {
            let EventKind::InputMessage(data) = &stored.event.kind else {
                return false;
            };
            data.blocks.iter().any(|block| {
                let InputBlock::Raw { text, .. } = block;
                if !text.contains(r#"kind="state""#) {
                    return false;
                }
                self.after
                    .is_none_or(|after| text.contains(&format!(r#"after="{after}""#)))
            })
        })
    }
}

impl GatedStore {
    pub fn fail_restore(&self) {
        self.fail_read.store(true, Ordering::SeqCst);
    }

    /// The case's ends of the gate: it waits on the first until the creation
    /// check runs, then writes its change and lets the second go once the
    /// Dispatcher consumed it.
    pub fn gate(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        *self.entered.lock().unwrap() = Some(entered);
        *self.release.lock().unwrap() = Some(held);
        (waiting, release)
    }

    /// Holds the next continuation input's publication: the first end fires
    /// once its append is committed, and sending on the second publishes it.
    /// Until then the Dispatcher has not delivered the input to the fold.
    pub fn hold_continuation(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, written) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        *self.published.lock().unwrap() = Some((entered, held));
        (written, release)
    }

    /// Holds the publication of the state record naming `after` (`None`: the
    /// next state record) and of every fact committed after it: the first end
    /// fires once the record's append is committed, and sending on the second
    /// publishes it and then the facts behind it.
    pub fn hold_state(
        &self,
        after: Option<u64>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, written) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        *self.state.lock().unwrap() = Some(StateHold {
            after,
            entered: Some(entered),
            release: Some(held),
            triggered: false,
            released: false,
            queue: VecDeque::new(),
        });
        (written, release)
    }

    /// Holds the next read whose single kind is `kind`: the first end fires
    /// once the read starts, and sending on the second lets it answer.
    pub fn hold_query(
        &self,
        kind: &str,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        self.hold_read(&[kind])
    }

    /// Holds the next read whose kind selection is exactly `kinds`, whatever
    /// its branch or direction: the first end fires once the read starts, and
    /// sending on the second lets it answer.
    pub fn hold_read(
        &self,
        kinds: &[&str],
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        self.hold_read_other_than(kinds, None)
    }

    /// [`Self::hold_read`] for a read on any branch but `branch`: the scan of a
    /// branch that is still loading, while another branch's reads go through.
    pub fn hold_read_other_than(
        &self,
        kinds: &[&str],
        branch: Option<&str>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, written) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        *self.read.lock().unwrap() = Some(ReadHold {
            kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            not_branch: branch.map(str::to_owned),
            entered,
            release: held,
        });
        (written, release)
    }
}

impl TimelineStore for GatedStore {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        if query_kinds(query.filter.as_ref()) == ["branch/start", "session/forked"]
            && self.fail_read.swap(false, Ordering::SeqCst)
        {
            return Box::pin(async { Err(AgentError::Storage("Goal restore read failed".into())) });
        }
        let creation_check = query_kinds(query.filter.as_ref()) == ["branch/create"]
            && matches!(query.select, dim_agent_runtime::QuerySelect::Exists);
        let armed = creation_check
            .then(|| {
                (
                    self.entered.lock().unwrap().take(),
                    self.release.lock().unwrap().take(),
                )
            })
            .and_then(|(entered, release)| entered.zip(release));
        let held = {
            let mut slot = self.read.lock().unwrap();
            match slot.as_ref() {
                Some(hold)
                    if query_kinds(query.filter.as_ref()) == hold.kinds
                        && hold
                            .not_branch
                            .as_ref()
                            .is_none_or(|branch| branch != &scope.key.branch_id) =>
                {
                    slot.take()
                }
                _ => None,
            }
        };
        Box::pin(async move {
            if let Some((entered, release)) = armed {
                let _ = entered.send(());
                let _ = release.await;
            }
            if let Some(ReadHold {
                entered, release, ..
            }) = held
            {
                let _ = entered.send(());
                let _ = release.await;
            }
            self.memory.query(scope, query, page).await
        })
    }

    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(session_id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(session_id)
    }
    fn release_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(session_id)
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Option<SessionConfig>>> {
        self.memory.session_config(session_id)
    }
    fn branches<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Vec<StoredBranch>>> {
        self.memory.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        let continuation = events.iter().any(|stored| {
            let EventKind::InputMessage(data) = &stored.event.kind else {
                return false;
            };
            data.blocks.first().is_some_and(|block| {
                let InputBlock::Raw { text, .. } = block;
                text.contains(r#"kind="continuation""#)
            })
        });
        if continuation && let Some((entered, held)) = self.published.lock().unwrap().take() {
            return self.memory.append(
                key,
                events,
                Box::new(move |result| {
                    tokio::spawn(async move {
                        let _ = entered.send(());
                        let _ = held.await;
                        publish(result);
                    });
                }),
            );
        }
        enum Held {
            /// This append is the held record: its publication waits for the
            /// case, then drains the queue in commit order.
            Trigger(
                tokio::sync::oneshot::Sender<()>,
                tokio::sync::oneshot::Receiver<()>,
            ),
            /// A fact committed after the held record: it publishes behind it.
            Behind,
        }
        let held = {
            let mut slot = self.state.lock().unwrap();
            match slot.as_mut() {
                Some(hold) if hold.released => None,
                Some(hold) if hold.triggered => Some(Held::Behind),
                Some(hold) if hold.matches(&events) => {
                    hold.triggered = true;
                    match (hold.entered.take(), hold.release.take()) {
                        (Some(entered), Some(release)) => Some(Held::Trigger(entered, release)),
                        _ => Some(Held::Behind),
                    }
                }
                _ => None,
            }
        };
        match held {
            None => self.memory.append(key, events, publish),
            Some(Held::Behind) => {
                let state = self.state.clone();
                self.memory.append(
                    key,
                    events,
                    Box::new(move |result| {
                        let mut slot = state.lock().unwrap();
                        match slot.as_mut() {
                            Some(hold) if !hold.released => {
                                hold.queue.push_back(Box::new(move || publish(result)));
                            }
                            _ => {
                                drop(slot);
                                publish(result);
                            }
                        }
                    }),
                )
            }
            Some(Held::Trigger(entered, release)) => {
                let state = self.state.clone();
                self.memory.append(
                    key,
                    events,
                    Box::new(move |result| {
                        tokio::spawn(async move {
                            let _ = entered.send(());
                            let _ = release.await;
                            let mut slot = state.lock().unwrap();
                            if let Some(hold) = slot.as_mut() {
                                hold.released = true;
                            }
                            publish(result);
                            loop {
                                let next = slot.as_mut().and_then(|hold| hold.queue.pop_front());
                                match next {
                                    Some(callback) => callback(),
                                    None => break,
                                }
                            }
                        });
                    }),
                )
            }
        }
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.batch_end(key, sequence)
    }

    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, dim_agent_runtime::Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

/// One Session over the part harness, and the handles a case needs.
pub struct Env {
    host: MinimalHost,
    pub session: Session,
    pub probe: Arc<Probe>,
    home: PathBuf,
    refusing: Option<Arc<RefusingStore>>,
    gated: Option<Arc<GatedStore>>,
}

impl Env {
    pub async fn start() -> Self {
        Self::start_inner(None, None, None).await
    }

    /// [`Self::start`] on a store that records every read Goal issues and can
    /// refuse one branch's queries; nothing is refused unless a case arms it.
    pub async fn start_refusing() -> Self {
        Self::start_inner(Some(Arc::new(RefusingStore::default())), None, None).await
    }

    /// [`Self::start`] on a store that can hold Goal's creation check.
    pub async fn start_gated() -> Self {
        Self::start_inner(None, Some(Arc::new(GatedStore::default())), None).await
    }

    pub fn refusing(&self) -> &RefusingStore {
        self.refusing.as_ref().expect("a refusing store")
    }

    /// The reads the store recorded so far (empty on a store-less [`Self`]).
    pub fn reads(&self) -> Vec<Read> {
        self.refusing().reads()
    }

    pub fn gated(&self) -> &GatedStore {
        self.gated.as_ref().expect("a gated store")
    }

    pub async fn start_store(store: Arc<dyn TimelineStore>) -> Self {
        Self::start_inner(None, None, Some(store)).await
    }

    async fn start_inner(
        refusing: Option<Arc<RefusingStore>>,
        gated: Option<Arc<GatedStore>>,
        store: Option<Arc<dyn TimelineStore>>,
    ) -> Self {
        let home = home();
        let probe = Arc::new(Probe::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_goal::install(&mut builder);
        dim_agent_capabilities::register_names(&mut builder);
        dim_agent_runtime::names::register::<SettingsService>(builder.catalog_mut(), None);
        builder
            .require_feature(Feature::new("SessionMethods").session(
                "SessionMethods",
                dim_agent_capabilities::methods::SessionMethodsPlugin::part,
            ))
            .require_feature(
                Feature::new("ToolRegistry").session("ToolRegistry", |names| {
                    Ok(Box::new(ToolRegistryPlugin::new(names)?))
                }),
            )
            .require_feature(Feature::new("PromptGuide").session("PromptGuide", |names| {
                Ok(Box::new(PromptGuidePlugin::new(names)?))
            }))
            .require_feature(
                Feature::new("TimelineTree")
                    .session("TimelineTree", |names| {
                        Ok(Box::new(TimelineTreePlugin::new(names)?))
                    })
                    .branch("TreeTimeline", |names| {
                        Ok(Box::new(TreeTimelinePlugin::new(names)?))
                    }),
            )
            .require_part(dim_goal::SESSION_PART)
            .require_part(dim_goal::BRANCH_PART)
            .require_feature(
                Feature::new("GoalLoop")
                    .branch("GoalLoop", |names| Ok(Box::new(LoopStub::new(names)?))),
            );
        let session_probe = probe.clone();
        builder.require_feature(
            Feature::new("GoalProbeSession").session("GoalProbeSession", move |names| {
                Ok(Box::new(ProbeSession::new(names, session_probe.clone())?))
            }),
        );
        let branch_probe = probe.clone();
        builder.require_feature(
            Feature::new("GoalProbeBranch").branch("GoalProbeBranch", move |names| {
                Ok(Box::new(ProbeBranch::new(names, branch_probe.clone())?))
            }),
        );
        defaults::tool_host(
            &mut builder,
            Arc::new(dim_agent_capabilities::adapters::MockTools),
        );
        if let Some(store) = store {
            builder.timeline_store(store);
        }
        if let Some(store) = &refusing {
            builder.timeline_store(store.clone());
        }
        if let Some(store) = &gated {
            builder.timeline_store(store.clone());
        }
        let catalog = builder.catalog().clone();
        let build = Build::default();
        let names = Names::copy(&catalog, &build);
        let settings = SettingsPlugin::new(&names, 0, vec![]).expect("settings plugin");
        let files = home.clone();
        let host = MinimalHost::start_with(&builder, move |root, _| {
            root.plugin(FileStoragePlugin::new(vec![files.clone()]));
            root.plugin(settings.clone());
            Ok(())
        })
        .await
        .expect("the part host assembles");
        let session = host
            .agent()
            .create_session(dim_agent_runtime::SessionConfig {
                cwd: home.to_string_lossy().into_owned(),
                tools: Some(vec!["goal_update".into(), "wait".into()]),
                ..Default::default()
            })
            .await
            .expect("the Session builds");
        let env = Self {
            host,
            session,
            probe,
            home,
            refusing,
            gated,
        };
        env.await_parts().await;
        env
    }

    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    /// The Settings document the Session reads `maxContinuations` from.
    pub fn set_limit(&self, limit: u64) {
        std::fs::write(
            self.home.join("goal.json"),
            format!("{{\"maxContinuations\":{limit}}}"),
        )
        .unwrap();
    }

    pub fn timeline(&self, branch: &str) -> Arc<Timeline> {
        self.probe.timeline(branch)
    }

    /// Commits one fact on `branch`, as the runtime would.
    pub fn emit(&self, branch: &str, kind: EventKind) -> Arc<AgentEvent> {
        self.timeline(branch)
            .append(kind.into())
            .unwrap_or_else(|error| panic!("append on {branch}: {error}"))
    }

    pub fn dispatcher(&self, branch: &str) -> Arc<Dispatcher> {
        self.probe.dispatcher(branch)
    }

    /// Waits until the branch's Dispatcher delivered `sequence` to every
    /// subscriber: the fold has consumed the fact.
    pub async fn dispatched(&self, branch: &str, sequence: u64) {
        let dispatcher = self.dispatcher(branch);
        tokio::time::timeout(Duration::from_secs(15), async {
            while dispatcher.sequence() < sequence {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the Dispatcher consumes the change");
    }

    /// One `session.goal.*` call through the assembled SessionMethods.
    pub async fn goal(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let name = format!("session.goal.{method}");
        let methods = self
            .session
            .session_service::<SessionMethods>()
            .expect("SessionMethods");
        let provider = methods.provider(&name).expect("the Goal method");
        let accepted = provider
            .accept(name, params, None, CancellationToken::new(), |_| {})
            .expect("the method accepts");
        accepted.reply.await.expect("the method replies")
    }

    /// Opens a real child branch of `parent` at its head, with `prompt` and
    /// `source`, as the branch tool or Goal's own creation does.
    pub async fn open_branch(&self, parent: &str, prompt: &str, source: BranchSource) -> String {
        let id = self
            .create_branch(parent, prompt, source)
            .await
            .id()
            .to_owned();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !self.probe.has(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the branch's parts activate");
        id
    }

    /// Opens a real child branch of `parent` and returns it before its parts
    /// finish loading, so a case can act inside that window: the handle commits
    /// through the branch's own Timeline.
    pub async fn create_branch(
        &self,
        parent: &str,
        prompt: &str,
        source: BranchSource,
    ) -> dim_agent_runtime::Branch {
        let at = self.timeline(parent).sequence();
        let command = self.probe.commands(parent);
        command
            .branch(
                parent.to_owned(),
                at,
                BranchOptions {
                    prompt: prompt.into(),
                    source,
                },
                CancellationToken::new(),
            )
            .await
            .expect("the branch opens")
    }

    /// Ends `id` with `source`, as another line or the host does.
    pub async fn end_branch(&self, id: &str, source: BranchSource) {
        self.probe
            .commands("main")
            .end(id.to_owned(), source)
            .await
            .expect("the branch ends");
    }

    /// Closes the Session and restores it from its store: every part loads
    /// again over the history it left, as a process restart does.
    pub async fn restore(self) -> Self {
        let Self {
            host,
            session,
            probe,
            home,
            refusing,
            gated,
        } = self;
        let id = session.id().to_owned();
        session.close().await.expect("the Session closes");
        let session = host
            .agent()
            .restore_session(id)
            .await
            .expect("the Session restores");
        let env = Self {
            host,
            session,
            probe,
            home,
            refusing,
            gated,
        };
        env.await_parts().await;
        env
    }

    /// The Session id the harness created.
    pub fn session_id(&self) -> &str {
        self.session.id()
    }

    pub async fn restore_failure(&self, id: &str) -> dim_agent_runtime::AgentError {
        self.host
            .agent()
            .restore_session(id.to_owned())
            .await
            .err()
            .expect("invalid durable recovery state must fail the Session restore")
    }

    /// Starts a second Session record `id` in the shared store, to be filled
    /// before anything loads it: the history a restore replays.
    pub async fn seed_new(&self, id: &str) -> Seed {
        let store = self.probe.store();
        let config = SessionConfig {
            cwd: self.home.to_string_lossy().into_owned(),
            tools: Some(vec!["goal_update".into(), "wait".into()]),
            ..Default::default()
        };
        store
            .create_session(id, &config)
            .await
            .expect("the seeded session record");
        Seed {
            root: Ctx::root().expect("a seeding root"),
            store,
            session: id.to_owned(),
            timelines: HashMap::new(),
        }
    }

    /// Closes this Session and restores the seeded Session `id` in its place.
    pub async fn restore_seeded(self, id: &str) -> Self {
        let Self {
            host,
            session,
            probe,
            home,
            refusing,
            gated,
        } = self;
        session.close().await.expect("the Session closes");
        let session = host
            .agent()
            .restore_session(id.to_owned())
            .await
            .expect("the seeded Session restores");
        let env = Self {
            host,
            session,
            probe,
            home,
            refusing,
            gated,
        };
        env.await_parts().await;
        env
    }

    /// Waits until the Session's parts are mounted and Goal's methods landed.
    async fn await_parts(&self) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self.probe.has("main")
                    && self
                        .probe
                        .tools
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|tools| tools.contributed("goal_update").is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("main and its parts activate");
        self.await_goal_methods().await;
    }

    /// Waits until the `session.goal.*` methods are registered.
    pub async fn await_goal_methods(&self) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let methods = self.session.session_service::<SessionMethods>();
                if methods
                    .as_ref()
                    .is_ok_and(|methods| methods.provider("session.goal.set").is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("session.goal.* registers");
    }

    /// Whether the harness has `id`'s Timeline: its parts are mounted.
    pub fn has_branch(&self, id: &str) -> bool {
        self.probe.has(id)
    }

    /// The input message on `branch` whose first block contains `needle`.
    pub async fn written_input(&self, branch: &str, needle: &str) -> InputMessageData {
        self.all_events(branch)
            .await
            .into_iter()
            .find_map(|event| match &event.kind {
                EventKind::InputMessage(data)
                    if data
                        .blocks
                        .first()
                        .is_some_and(|block| raw_text(block).contains(needle)) =>
                {
                    Some(data.clone())
                }
                _ => None,
            })
            .expect("the input")
    }

    /// The `branch/create` facts on `branch`, oldest first.
    pub async fn creates(&self, branch: &str) -> Vec<dim_agent_types::BranchCreateData> {
        self.all_events(branch)
            .await
            .into_iter()
            .filter_map(|event| match &event.kind {
                EventKind::BranchCreate(data) => Some(data.clone()),
                _ => None,
            })
            .collect()
    }

    /// The `branch/ended` facts on `branch`, oldest first.
    pub async fn ended(&self, branch: &str) -> Vec<dim_agent_types::BranchEndedData> {
        self.all_events(branch)
            .await
            .into_iter()
            .filter_map(|event| match &event.kind {
                EventKind::BranchEnded(data) => Some(data.clone()),
                _ => None,
            })
            .collect()
    }

    /// The `goal_update` tool through the assembled ToolRegistry.
    pub async fn tool(&self, branch: &str, input: Value) -> ToolResult {
        self.probe
            .tools()
            .contributed("goal_update")
            .expect("goal_update is registered")
            .handler
            .execute(
                ToolCall {
                    id: "goal-call".into(),
                    name: "goal_update".into(),
                    input: input.as_object().cloned().unwrap_or_default(),
                },
                ToolExecutionContext {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                    branch_id: branch.into(),
                },
                CancellationToken::new(),
            )
            .await
            .expect("the tool runs")
    }

    /// The tool declarations the model would see.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.probe.tools().definitions()
    }

    /// The guide entry composition for `branch`: what the model's System
    /// carries.
    pub fn guide(&self, branch: &str) -> String {
        self.probe.guide().compose(branch)
    }

    /// The branch's live keyed blocks at its head, by the rule `session.keys`
    /// serves (keyed-blocks §2.3; `scan_keys` at
    /// crates/dim-app-server/src/connection/tree.rs:707): the newest live block
    /// of each key on the branch's own segment, ascending by (sequence, block).
    /// Returns each key with its raw block text.
    pub async fn keys(&self, branch: &str) -> Vec<(String, String)> {
        let mut found: HashMap<String, (u64, usize, String)> = HashMap::new();
        let mut closed: HashSet<String> = HashSet::new();
        let mut deleted: HashSet<String> = HashSet::new();
        for event in self.all_events(branch).await.iter().rev() {
            match &event.kind {
                EventKind::BranchStart(_) | EventKind::SessionForked(_) => {
                    break;
                }
                EventKind::InputDeleted(data) => {
                    deleted.insert(data.input_id.clone());
                }
                EventKind::InputMessage(data) if !deleted.contains(&event.id) => {
                    for (index, block) in data.blocks.iter().enumerate().rev() {
                        let InputBlock::Raw {
                            text,
                            key: Some(key),
                            retire,
                        } = block
                        else {
                            continue;
                        };
                        if found.contains_key(key) {
                            continue;
                        }
                        if *retire {
                            closed.insert(key.clone());
                            continue;
                        }
                        if closed.contains(key) {
                            continue;
                        }
                        found.insert(key.clone(), (event.sequence, index, text.clone()));
                    }
                }
                _ => {}
            }
        }
        let mut entries: Vec<(String, (u64, usize, String))> = found.into_iter().collect();
        entries.sort_by_key(|(_, (sequence, index, _))| (*sequence, *index));
        entries
            .into_iter()
            .map(|(key, (_, _, text))| (key, text))
            .collect()
    }

    /// The Goal inputs on `branch`, in order.
    pub async fn writes(&self, branch: &str) -> Vec<Block> {
        self.all_events(branch)
            .await
            .into_iter()
            .filter_map(|event| match &event.kind {
                EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                    let InputBlock::Raw { text, .. } = block;
                    text.starts_with("<dim-goal ").then(|| Block {
                        id: event.id.clone(),
                        sequence: event.sequence,
                        submission_id: data.submission_id.clone(),
                        marker: text.lines().next().unwrap().to_owned(),
                        text: text.clone(),
                        blocks: data.blocks.iter().map(raw_text).collect(),
                        mode: data.effective_mode(),
                    })
                }),
                _ => None,
            })
            .collect()
    }

    /// Waits until a Goal write on `branch` names `needle`; returns it.
    pub async fn wait_for(&self, branch: &str, needle: &str) -> Block {
        let mut last = Vec::new();
        let found = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                last = self.writes(branch).await;
                if let Some(block) = last.iter().find(|block| block.text.contains(needle)) {
                    return block.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        match found {
            Ok(block) => block,
            Err(_) => {
                let texts: Vec<&str> = last.iter().map(|block| block.marker.as_str()).collect();
                panic!("no Goal write with {needle} on {branch}; writes: {texts:?}")
            }
        }
    }

    /// The inputs on `branch` the Loop has neither consumed nor the operator
    /// deleted: what a turn takes in, Append mode aside.
    pub async fn pending_inputs(&self, branch: &str) -> Vec<String> {
        use std::collections::HashSet;
        let events = self.all_events(branch).await;
        let consumed: HashSet<&str> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::InputConsumed(data) => Some(data.input_id.as_str()),
                _ => None,
            })
            .collect();
        let deleted: HashSet<&str> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::InputDeleted(data) => Some(data.input_id.as_str()),
                _ => None,
            })
            .collect();
        events
            .iter()
            .filter(|event| {
                matches!(&event.kind, EventKind::InputMessage(data) if data.effective_mode() != InputMode::Append)
                    && !consumed.contains(event.id.as_str())
                    && !deleted.contains(event.id.as_str())
            })
            .map(|event| event.id.clone())
            .collect()
    }

    /// Every fact on `branch`, oldest first, read from the store so an ended
    /// line stays readable at its fixed head.
    pub async fn all_events(&self, branch: &str) -> Vec<Arc<AgentEvent>> {
        let store = self.probe.store();
        let key = TimelineKey {
            session_id: self.session.id().to_owned(),
            branch_id: branch.to_owned(),
        };
        let head = store.head(&key).await.expect("the branch's head");
        if head == 0 {
            return Vec::new();
        }
        store
            .read(&key, 1..=head)
            .await
            .expect("the branch's facts")
            .into_iter()
            .map(|record| record.event)
            .collect()
    }

    /// The Goal writes after the branch has been quiet for a while: the way a
    /// case asserts that a decision wrote nothing.
    pub async fn settled(&self, branch: &str) -> Vec<Block> {
        let mut last = Vec::new();
        let mut stable = 0;
        for _ in 0..600 {
            let writes = self.writes(branch).await;
            if writes.len() == last.len() {
                stable += 1;
                if stable >= 10 {
                    return writes;
                }
            } else {
                stable = 0;
                last = writes;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        last
    }

    /// `turn/start`, one `input/consumed` per input, `turn/end completed`:
    /// what the Loop writes around a turn.
    pub fn turn(&self, branch: &str, inputs: &[&Block]) -> Arc<AgentEvent> {
        let ids: Vec<&str> = inputs.iter().map(|input| input.id.as_str()).collect();
        self.turn_ids(branch, &ids)
    }

    pub fn turn_ids(&self, branch: &str, inputs: &[&str]) -> Arc<AgentEvent> {
        self.turn_with_ids(branch, inputs, "completed")
    }

    pub fn turn_with(&self, branch: &str, inputs: &[&Block], status: &str) -> Arc<AgentEvent> {
        let ids: Vec<&str> = inputs.iter().map(|input| input.id.as_str()).collect();
        self.turn_with_ids(branch, &ids, status)
    }

    pub fn turn_with_ids(&self, branch: &str, inputs: &[&str], status: &str) -> Arc<AgentEvent> {
        let turn_id = format!("turn-{}", uuid::Uuid::new_v4());
        self.turn_steps(branch, &turn_id, inputs, status, &[])
    }

    /// `turn/start`, one `input/consumed` per input, each request's
    /// `step/model-start` and text deltas, then `turn/end`: what the Loop
    /// writes for the turn `turn_id`.
    pub fn turn_steps(
        &self,
        branch: &str,
        turn_id: &str,
        inputs: &[&str],
        status: &str,
        steps: &[(&str, &[&str])],
    ) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::TurnStart(dim_agent_types::TurnStartData {
                turn_id: turn_id.to_owned(),
            }),
        );
        for input in inputs {
            self.emit(
                branch,
                EventKind::InputConsumed(dim_agent_types::InputConsumedData {
                    input_id: (*input).to_owned(),
                }),
            );
        }
        for (request, deltas) in steps {
            self.emit(
                branch,
                EventKind::ModelStart(StepData {
                    turn_id: turn_id.to_owned(),
                    request_id: (*request).to_owned(),
                }),
            );
            for delta in *deltas {
                self.emit(
                    branch,
                    EventKind::Text(DeltaData {
                        turn_id: turn_id.to_owned(),
                        request_id: (*request).to_owned(),
                        delta: (*delta).to_owned(),
                    }),
                );
            }
        }
        self.emit(branch, turn_end_with(turn_id, status))
    }

    /// A turn that has not ended yet: `turn/start`.
    pub fn turn_start(&self, branch: &str, turn_id: &str) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::TurnStart(dim_agent_types::TurnStartData {
                turn_id: turn_id.to_owned(),
            }),
        )
    }

    /// `input/consumed`: the Loop took `input` into the open turn.
    pub fn consume(&self, branch: &str, input: &str) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::InputConsumed(dim_agent_types::InputConsumedData {
                input_id: input.to_owned(),
            }),
        )
    }

    /// `turn/end` for the open `turn_id`.
    pub fn end_turn(&self, branch: &str, turn_id: &str, status: &str) -> Arc<AgentEvent> {
        self.emit(branch, turn_end_with(turn_id, status))
    }

    /// The `step/tool-call` a turn writes when the model calls a tool.
    pub fn tool_call(&self, branch: &str, turn_id: &str) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::ToolCall(dim_agent_types::ToolCallData {
                turn_id: turn_id.to_owned(),
                request_id: "request".into(),
                calls: vec![ToolCall {
                    id: format!("call-{}", uuid::Uuid::new_v4()),
                    name: "echo".into(),
                    input: serde_json::Map::new(),
                }],
            }),
        )
    }

    /// The `exec/started` a background tool writes for `task_id`.
    pub fn exec_started(&self, branch: &str, turn_id: &str, task_id: &str) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::ExecStarted(dim_agent_types::ExecStartedData {
                turn_id: turn_id.to_owned(),
                request_id: "request".into(),
                call_id: format!("call-{task_id}"),
                task_id: task_id.to_owned(),
                description: "background work".into(),
                output_path: "/dev/null".into(),
            }),
        )
    }

    /// The `exec/ended` that closes `task_id` with `status`.
    pub fn exec_ended(&self, branch: &str, task_id: &str, status: &str) -> Arc<AgentEvent> {
        let status = match status {
            "completed" => dim_agent_types::ExecStatus::Completed,
            "failed" => dim_agent_types::ExecStatus::Failed,
            "cancelled" => dim_agent_types::ExecStatus::Cancelled,
            other => panic!("unknown exec status {other}"),
        };
        self.emit(
            branch,
            EventKind::ExecEnded(dim_agent_types::ExecEndedData {
                task_id: task_id.to_owned(),
                status,
                result: ToolResult::default(),
            }),
        )
    }

    /// A completed turn that takes in `inputs` and makes one tool call, as the
    /// Loop writes a turn that used a tool.
    pub fn tool_turn(&self, branch: &str, inputs: &[&Block]) -> Arc<AgentEvent> {
        let turn_id = format!("turn-{}", uuid::Uuid::new_v4());
        self.turn_start(branch, &turn_id);
        self.tool_call(branch, &turn_id);
        for input in inputs {
            self.consume(branch, &input.id);
        }
        self.end_turn(branch, &turn_id, "completed")
    }

    /// An input message of literal blocks, committed on `branch`.
    pub fn write_message(
        &self,
        branch: &str,
        blocks: Vec<InputBlock>,
        mode: InputMode,
    ) -> Arc<AgentEvent> {
        self.emit(branch, message(blocks, mode))
    }

    pub fn interrupt(&self, branch: &str) -> Arc<AgentEvent> {
        self.emit(
            branch,
            EventKind::StopInterrupt(dim_agent_types::StopInterruptData {
                reason: "user".into(),
                scope: dim_agent_types::InterruptScope::Turn,
            }),
        )
    }

    pub fn delete(&self, branch: &str, input: &Block) {
        self.delete_id(branch, &input.id);
    }

    /// `input/deleted` for an input given by id.
    pub fn delete_id(&self, branch: &str, input: &str) {
        self.emit(
            branch,
            EventKind::InputDeleted(dim_agent_types::InputDeletedData {
                input_id: input.to_owned(),
            }),
        );
    }

    /// A `set` through the Session method, waited for in the log.
    pub async fn set(&self, objective: &str) -> Block {
        self.goal("set", serde_json::json!({"objective": objective}))
            .await
            .expect("set writes");
        self.wait_for("main", r#"op="set""#).await
    }

    pub async fn close(self) {
        self.host.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The `turn/end` fact a case writes.
pub fn turn_end(status: &str) -> EventKind {
    turn_end_with(&format!("turn-{}", uuid::Uuid::new_v4()), status)
}

/// The `turn/end` fact of `turn_id` with `status`.
pub fn turn_end_with(turn_id: &str, status: &str) -> EventKind {
    let outcome = match status {
        "completed" => dim_agent_types::TurnOutcome::Completed {
            reason: "stop".into(),
        },
        "failed" => dim_agent_types::TurnOutcome::Failed {
            error: "failed".into(),
        },
        "cancelled" => dim_agent_types::TurnOutcome::Cancelled {
            reason: "cancelled".into(),
        },
        other => panic!("unknown turn status {other}"),
    };
    EventKind::TurnEnd(dim_agent_types::TurnEndData {
        turn_id: turn_id.to_owned(),
        outcome,
    })
}

/// One input message of literal blocks, the way a restored or foreign writer
/// leaves one.
pub fn message(blocks: Vec<InputBlock>, mode: InputMode) -> EventKind {
    EventKind::InputMessage(input_message(blocks, mode))
}

/// The [`InputMessageData`] behind [`message`], for committing through a
/// [`dim_agent_runtime::Branch`] handle.
pub fn input_message(blocks: Vec<InputBlock>, mode: InputMode) -> InputMessageData {
    InputMessageData {
        handover: None,
        submission_id: Some(format!("test:{}", uuid::Uuid::new_v4())),
        user: None,
        blocks,
        mode: Some(mode),
    }
}

/// A raw block with literal text.
pub fn raw(text: &str, key: Option<&str>, retire: bool) -> InputBlock {
    InputBlock::Raw {
        text: text.into(),
        key: key.map(str::to_owned),
        retire,
    }
}

/// A user input, the way the operator writes one.
pub fn user(text: &str, mode: InputMode) -> EventKind {
    EventKind::InputMessage(InputMessageData {
        user: Some(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text { text: text.into() }],
        }),
        mode: Some(mode),
        ..Default::default()
    })
}

/// A user input carrying extra blocks, the way another plugin writes one.
pub fn user_with_blocks(text: &str, mode: InputMode, blocks: Vec<InputBlock>) -> EventKind {
    EventKind::InputMessage(InputMessageData {
        user: Some(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text { text: text.into() }],
        }),
        blocks,
        mode: Some(mode),
        ..Default::default()
    })
}

/// One Goal input the branch holds.
#[derive(Debug, Clone)]
pub struct Block {
    pub id: String,
    pub sequence: u64,
    /// The input's submission id (`goal:<uuid>` for every Goal write).
    pub submission_id: Option<String>,
    /// The `<dim-goal ...>` marker line.
    pub marker: String,
    pub text: String,
    /// Every block of the input, in order.
    pub blocks: Vec<String>,
    pub mode: InputMode,
}

impl Block {
    /// The decoded value after `label`, up to the block's last blank line.
    pub fn value(&self, label: &str) -> String {
        let start = self.text.find(&format!("\n{label}\n")).unwrap() + label.len() + 2;
        let end = self.text.rfind("\n\n").unwrap();
        unescape(&self.text[start..end])
    }
    /// The decoded body after `label`, up to the closing marker.
    pub fn body(&self, label: &str) -> String {
        let start = self.text.find(&format!("\n{label}\n")).unwrap() + label.len() + 2;
        let end = self.text.rfind("\n</dim-goal>").unwrap();
        unescape(&self.text[start..end])
    }
}

fn raw_text(block: &InputBlock) -> String {
    let InputBlock::Raw { text, .. } = block;
    text.clone()
}

pub fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

fn home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("dim-goal-parts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    // FileStorage opens every path component with O_NOFOLLOW, and on macOS the
    // temp directory sits below `/var`, a symbolic link.
    let home = home.canonicalize().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    home
}

pub fn goal_state(objective: Option<&str>, state: Value, evaluator: bool) -> EventKind {
    EventKind::Extension(dim_agent_types::ExtensionEvent::new("goal", "state", serde_json::json!({"schema":"goal-state/v1", "operation":if objective.is_some(){"set"}else{"clear"}, "objective":objective, "state":state, "judge":if evaluator{"evaluator"}else{"model"}, "input_id":null})).unwrap())
}
