use crate::{
    host::{HostError, HostFuture, ToolExecutionContext},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{
    AgentError, Branch, BranchLifecycle, BranchOptions, Delivery, Dispatcher, Names, Result,
    SessionIdentity, StoredBranch, StoredEvent, Timeline, TimelineContext, TimelineFactory,
    TimelineIdentity, TimelineInfo, TimelineKey, TimelineStore, TimelineTreeControl,
};
use dim_agent_types::*;
use futures_util::{FutureExt, future::join_all};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::service;
use rutis::{Ctx, Error};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Events per page of a restored child's creation check; the events a
/// created child wrote while it opened fit in one.
const CREATION_PAGE: usize = 32;

const TAIL_EVENTS: u64 = 32;

/// How a loaded branch begins on its Timeline.
enum Opening {
    /// A new branch: the root of a new Session, or a child with its first
    /// input.
    Start {
        start: BranchStartData,
        first: Option<InputMessageData>,
    },
    /// A stored branch loaded again at its stored head. While it opens, its
    /// build reads its history up to that head, the history its capabilities
    /// rebuild from, and reports the facts it holds about its own lines: the
    /// creates it saw fail, and the source each create recorded. The restore
    /// checks its children against them.
    Resume {
        head: u64,
        lines: oneshot::Sender<LineFacts>,
    },
}

#[derive(Default)]
struct LineFacts {
    /// The `branch/create` sequences the line recorded as failed.
    failed_creates: BTreeSet<u64>,
    /// Each child it created, with the source that create recorded.
    sources: BTreeMap<String, BranchSource>,
    /// The children whose end it recorded.
    ended: BTreeSet<String>,
}

struct Node {
    info: TimelineInfo,
    /// The built branch, before and after publication.
    context: Option<Arc<TimelineContext>>,
    ready: watch::Sender<Option<Result<Branch>>>,
    started: bool,
}
struct EndedLine {
    head: u64,
    info: TimelineInfo,
}
/// A line whose `branch/end` is committed and whose close has not completed.
struct PendingEnd {
    /// The sequence of `branch/end` on the line: the result's `endEvent`.
    sequence: u64,
    data: BranchEndData,
}
struct TreeState {
    nodes: BTreeMap<String, Node>,
    ended: BTreeMap<String, EndedLine>,
    /// Lines waiting for the turn end that a tool-sourced end belongs to.
    pending: BTreeMap<String, PendingEnd>,
    closing: bool,
    cleanup_errors: Vec<AgentError>,
}
pub struct TimelineTree {
    weak: Weak<Self>,
    state: Mutex<TreeState>,
    factory: Arc<TimelineFactory>,
    store: Arc<dyn TimelineStore>,
    session_id: String,
    cancel: CancellationToken,
    builds: TaskTracker,
    close_result: watch::Sender<Option<Result<()>>>,
    close_task: Mutex<Option<JoinHandle<()>>>,
    close_started: AtomicBool,
    /// Wakes the callers waiting for an end to complete or to wait for a turn.
    end_notify: tokio::sync::Notify,
}
/// Shared with Timeline roots; never owns the tree or its child roots.
pub struct TreeCommands {
    tree: Weak<TimelineTree>,
}
#[service]
impl TreeCommands {
    #[service(skip)]
    pub fn get(&self, id: &str) -> Result<Branch> {
        let tree = self
            .tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?;
        if tree.ended_head(id).is_some() {
            return Err(AgentError::BranchEnded(id.into()));
        }
        tree.owned_context(id)
            .map(|context| context.branch())
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))
    }
    pub fn list(&self) -> Vec<TimelineInfo> {
        self.tree
            .upgrade()
            .map(|tree| tree.list())
            .unwrap_or_default()
    }
    #[service(skip)]
    pub async fn branch(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: CancellationToken,
    ) -> Result<Branch> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .branch(parent, at, options, admission)
            .await
    }
    // The data methods below spell out `std::result::Result` for rutis'
    // export generator, which resolves an alias's item type where the alias
    // is defined (arcships/rutis#463).
    /// The directory record of branch `id`, from its reservation on, so a
    /// part that loads with its own line reads it during its replay.
    pub fn info(&self, id: String) -> std::result::Result<TimelineInfo, AgentError> {
        let tree = self
            .tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?;
        tree.info(&id).ok_or(AgentError::UnknownBranch(id))
    }
    /// Commits `input` to branch `id` through its validating `Branch::input`
    /// and returns the committed event, or the input already committed with
    /// its submission id.
    pub async fn input(
        &self,
        id: String,
        input: InputMessageData,
    ) -> std::result::Result<Arc<AgentEvent>, AgentError> {
        self.get(&id)?.input(input).await
    }
    /// Ends the line `id` with `source`; only the target is checked (§10).
    pub async fn end(
        &self,
        id: String,
        source: BranchSource,
    ) -> std::result::Result<(), AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .end(&id, source)
            .await
    }
    /// One page of `id`'s committed batches; an ended line reads at its
    /// fixed head.
    pub async fn page(
        &self,
        id: String,
        after: u64,
        limit: usize,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .page(&id, after, limit)
            .await
    }
    /// The head a read of `id` ends at: fixed once ended, published while
    /// open.
    #[service(skip)]
    fn head(&self, id: &str) -> Option<u64> {
        self.tree.upgrade()?.head(id)
    }
    /// Creates a child of `parent` cut at `at` and returns its id.
    /// `cancelled` is the caller's cancellation as an explicit argument, the
    /// form every cross-language contract uses for it (C4's `cancelled`): the
    /// creation's admission is cancelled, as [`Self::branch`]'s token does,
    /// once the future it returns completes, or fails because the caller is
    /// gone. Dropping the call cancels it too. TimelineTree records the outcome.
    /// Cancellation is observed through that future, so it ends only a
    /// creation still open when the future completes: a child already built
    /// returns its id, however early the caller's cancellation completed on
    /// its own side.
    pub async fn create(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        cancelled: Arc<
            dyn Fn() -> BoxFuture<'static, std::result::Result<(), Error>> + Send + Sync,
        >,
    ) -> Result<String> {
        let admission = CancellationToken::new();
        let cancel_on_drop = admission.clone().drop_guard();
        let caller = admission.clone();
        let created = self.branch(parent, at, options, admission);
        tokio::pin!(created);
        let created = tokio::select! {
            result = &mut created => result,
            _ = cancelled() => {
                caller.cancel();
                created.await
            }
        };
        cancel_on_drop.disarm();
        Ok(created?.id().to_owned())
    }
}
impl TimelineTree {
    fn create_failed(info: &TimelineInfo, error: &AgentError) -> BranchCreateFailedData {
        let branch = info.id.clone();
        let create_event = info.create_anchor.expect("child creation anchor");
        match error {
            AgentError::Cancelled(_) => BranchCreateFailedData::Cancelled {
                branch,
                create_event,
                reason: "branch creation cancelled".into(),
            },
            _ => BranchCreateFailedData::Failed {
                branch,
                create_event,
                error: "branch creation failed".into(),
            },
        }
    }
    fn record_create_failed(&self, info: &TimelineInfo, error: &AgentError) -> Result<()> {
        let parent = info.parent.as_deref().expect("failed child has parent");
        let parent = self
            .owned_context(parent)
            .ok_or_else(|| AgentError::UnknownBranch(parent.into()))?;
        parent
            .timeline()
            .append(EventKind::BranchCreateFailed(Self::create_failed(info, error)).into())?;
        Ok(())
    }
    fn complete_end(&self, info: &TimelineInfo, end_event: u64, data: BranchEndData) -> Result<()> {
        let Some(parent) = info.parent.as_deref() else {
            return Ok(());
        };
        let parent = self
            .owned_context(parent)
            .ok_or_else(|| AgentError::UnknownBranch(parent.into()))?;
        parent
            .timeline()
            .append(
                EventKind::BranchEnded(BranchEndedData {
                    branch: info.id.clone(),
                    end_event,
                    source: data.source,
                    ancestor: data.ancestor,
                })
                .into(),
            )
            .map(|_| ())
    }
    /// The lines whose end `branch` recorded, from its stored history.
    async fn stored_ends(&self, branch: &str, head: u64) -> Result<BTreeSet<String>> {
        if head == 0 {
            return Ok(BTreeSet::new());
        }
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: branch.into(),
        };
        Ok(self
            .store
            .read(&key, 1..=head)
            .await?
            .into_iter()
            .filter_map(|record| match &record.event.kind {
                EventKind::BranchEnded(data) => Some(data.branch.clone()),
                _ => None,
            })
            .collect())
    }
    /// The stored lines below `id`, deepest first.
    fn stored_descendants(id: &str, stored: &BTreeMap<String, StoredBranch>) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![(id.to_owned(), 0usize)];
        while let Some((parent, depth)) = pending.pop() {
            for branch in stored
                .values()
                .filter(|branch| branch.info.parent.as_deref() == Some(parent.as_str()))
            {
                found.push((branch.info.id.clone(), depth + 1));
                pending.push((branch.info.id.clone(), depth + 1));
            }
        }
        found.sort_by_key(|found| std::cmp::Reverse(found.1));
        found.into_iter().map(|(id, _)| id).collect()
    }
    /// Completes the ends below a line whose own end was interrupted,
    /// deepest first, each result on the line's stored parent: that line is
    /// not opened, so the record goes to its stored history and its fixed
    /// head follows (design §10, §17 R1).
    async fn complete_stored_ends(
        &self,
        line: &TimelineInfo,
        stored: &mut BTreeMap<String, StoredBranch>,
    ) -> Result<()> {
        let mut recorded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for id in Self::stored_descendants(&line.id, stored) {
            let parent = stored[&id]
                .info
                .parent
                .clone()
                .expect("a stored descendant has a parent");
            if !recorded.contains_key(&parent) {
                let head = stored[&parent].sequence;
                recorded.insert(parent.clone(), self.stored_ends(&parent, head).await?);
            }
            if recorded[&parent].contains(&id) {
                continue;
            }
            let head = stored[&id].sequence;
            let Some((end_event, data)) = self.own_end(&id, head).await? else {
                continue;
            };
            let parent_head = stored[&parent].sequence;
            let head = self
                .append_stored_result(
                    &parent,
                    parent_head,
                    BranchEndedData {
                        branch: id.clone(),
                        end_event,
                        source: data.source,
                        ancestor: data.ancestor,
                    },
                )
                .await?;
            stored
                .get_mut(&parent)
                .expect("the parent is stored")
                .sequence = head;
        }
        Ok(())
    }
    /// Appends one result on a line that restore keeps closed, and returns
    /// the line's new head.
    async fn append_stored_result(
        &self,
        branch: &str,
        head: u64,
        data: BranchEndedData,
    ) -> Result<u64> {
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: branch.into(),
        };
        let sequence = head + 1;
        let event = Arc::new(AgentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            create_at: dim_agent_runtime::core::unix_millis(),
            tags: None,
            kind: EventKind::BranchEnded(data),
        });
        let (published, wait) = oneshot::channel();
        self.store.append(
            key,
            vec![StoredEvent {
                event,
                trace: None,
                batch_end: sequence,
            }],
            Box::new(move |result| {
                let _ = published.send(result);
            }),
        )?;
        wait.await.expect("the store publishes each append once")?;
        Ok(sequence)
    }
    fn settle_failed(&self, info: &TimelineInfo, error: AgentError) -> AgentError {
        match self.record_create_failed(info, &error) {
            Ok(()) => error,
            Err(record_error) => {
                self.state
                    .lock()
                    .unwrap()
                    .cleanup_errors
                    .push(record_error.clone());
                error.with_cleanup(record_error)
            }
        }
    }
    /// Whether a stored child's own facts settle its creation: `branch/start`
    /// followed by the first input, which is Queue. What Prompt writes while
    /// `branch/start` dispatches is Append and does not count. Reads from the
    /// start a page at a time and stops at that input, so a created child
    /// reads only the events of its opening, not its whole history.
    async fn created(&self, branch: &str, head: u64) -> Result<bool> {
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: branch.into(),
        };
        let (mut after, mut started) = (0, false);
        while after < head {
            let page = self
                .store
                .read_page(&key, after, CREATION_PAGE, head)
                .await?;
            for record in &page {
                match &record.event.kind {
                    EventKind::BranchStart(_) => started = true,
                    EventKind::InputMessage(data)
                        if started && data.effective_mode() == InputMode::Queue =>
                    {
                        return Ok(true);
                    }
                    _ => {}
                }
            }
            after = page.last().map_or(head, |record| record.event.sequence);
        }
        Ok(false)
    }
    fn new(
        factory: Arc<TimelineFactory>,
        store: Arc<dyn TimelineStore>,
        session_id: String,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            state: Mutex::new(TreeState {
                nodes: BTreeMap::new(),
                ended: BTreeMap::new(),
                pending: BTreeMap::new(),
                closing: false,
                cleanup_errors: Vec::new(),
            }),
            factory,
            store,
            session_id,
            cancel: CancellationToken::new(),
            builds: TaskTracker::new(),
            close_result: watch::channel(None).0,
            close_task: Mutex::new(None),
            close_started: AtomicBool::new(false),
            end_notify: tokio::sync::Notify::new(),
        })
    }
    fn owned_context(&self, id: &str) -> Option<Arc<TimelineContext>> {
        self.state
            .lock()
            .unwrap()
            .nodes
            .get(id)
            .and_then(|node| node.context.clone())
    }
    fn reserve(
        &self,
        state: &mut TreeState,
        info: TimelineInfo,
    ) -> Result<watch::Receiver<Option<Result<Branch>>>> {
        if state.closing {
            return Err(AgentError::Closed("TimelineTree".into()));
        }
        if state.nodes.contains_key(&info.id) {
            return Err(AgentError::InvalidInput(
                "branch identity is already occupied".into(),
            ));
        }
        let (ready, receiver) = watch::channel(None);
        state.nodes.insert(
            info.id.clone(),
            Node {
                info,
                context: None,
                ready,
                started: false,
            },
        );
        Ok(receiver)
    }
    fn launch(
        &self,
        state: &mut TreeState,
        id: String,
        ancestors: Vec<(String, u64)>,
        validate: Option<(Timeline, u64)>,
        opening: Opening,
    ) {
        let node = state.nodes.get_mut(&id).expect("reserved branch");
        node.started = true;
        let ready = node.ready.clone();
        let info = node.info.clone();
        let parent_ready = info.parent.as_ref().map(|parent| {
            state
                .nodes
                .get(parent)
                .expect("owned parent")
                .ready
                .subscribe()
        });
        let owner = self.weak.upgrade().expect("tree owns branch creation");
        self.builds.spawn(async move {
            let mut acquired = None;
            let mut cleanup_failure = None;
            let mut started = false;
            let mut outcome = AssertUnwindSafe(async {
                if let Some(parent_ready) = parent_ready {
                    tokio::select! {
                        result = Self::await_branch(parent_ready) => { result?; }
                        _ = owner.cancel.cancelled() => {
                            return Err(AgentError::Cancelled("branch creation".into()));
                        }
                    }
                }
                if let Some((timeline, through)) = validate {
                    let context = tokio::select! {
                        result = tokio::time::timeout(owner.factory.startup_timeout(), crate::history::read_context(&timeline, through)) =>
                            result.map_err(|_| AgentError::Build("branch opening timed out".into()))??,
                        _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                    };
                    crate::history::validate_context_boundary(&context)
                        .map_err(AgentError::InvalidInput)?;
                }
                let built = owner
                    .factory
                    .build(id.clone(), ancestors, owner.cancel.child_token())
                    .await;
                let context = match built {
                    Err(error) => {
                        cleanup_failure = error.cleanup.clone();
                        return Err(error.into());
                    }
                    Ok(context) => context,
                };
                acquired = Some(context.clone());
                owner
                    .state
                    .lock()
                    .unwrap()
                    .nodes
                    .get_mut(&id)
                    .expect("building branch")
                    .context = Some(context.clone());
                let (opening, first) = match opening {
                    Opening::Start { start, first } => (EventKind::BranchStart(start), first),
                    Opening::Resume { head, lines } => {
                        let history = tokio::select! {
                            result = tokio::time::timeout(owner.factory.startup_timeout(), context.timeline().local(..=head)) =>
                                result.map_err(|_| AgentError::Build("branch opening timed out".into()))??,
                            _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                        };
                        let mut facts = LineFacts::default();
                        for event in &history {
                            match &event.kind {
                                EventKind::BranchCreateFailed(
                                    BranchCreateFailedData::Failed { create_event, .. }
                                    | BranchCreateFailedData::Cancelled { create_event, .. },
                                ) => {
                                    facts.failed_creates.insert(*create_event);
                                }
                                EventKind::BranchCreate(data) => {
                                    facts.sources.insert(data.branch.clone(), data.source.clone());
                                }
                                EventKind::BranchEnded(data) => {
                                    facts.ended.insert(data.branch.clone());
                                }
                                _ => {}
                            }
                        }
                        let _ = lines.send(facts);
                        (EventKind::BranchResume(BranchResumeData {}), None)
                    }
                };
                let opened = context.timeline().append(opening.into())?;
                tokio::select! {
                    result = context.finish_opening(opened.sequence, owner.factory.startup_timeout()) => result?,
                    _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                }
                if let Some(first) = first {
                    context.branch().input(first).await?;
                    started = true;
                }
                if owner.cancel.is_cancelled() {
                    return Err(AgentError::Cancelled("branch creation".into()));
                }
                Ok(context.branch())
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| {
                Err(AgentError::fault(
                    "TimelineTree build",
                    crate::host::panic_message(panic),
                ))
            });
            if let Ok(branch) = &outcome {
                let state = owner.state.lock().unwrap();
                if !state.closing {
                    if let Some(context) = &acquired {
                        context.activate();
                    }
                    ready.send_replace(Some(Ok(branch.clone())));
                    return;
                }
                drop(state);
                outcome = Err(AgentError::Cancelled("branch creation".into()));
            }
            if let (Err(error), Some(context)) = (&outcome, acquired) {
                let cleanup = AssertUnwindSafe(async { context.close().await })
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|panic| {
                        Err(AgentError::fault(
                            "TimelineTree rollback",
                            crate::host::panic_message(panic),
                        ))
                    });
                if let Err(cleanup) = cleanup {
                    cleanup_failure = Some(cleanup.clone());
                    outcome = Err(error.clone().with_cleanup(cleanup));
                }
            }
            if info.parent.is_some()
                && !started
                && let Err(error) = &outcome
            {
                outcome = Err(owner.settle_failed(&info, error.clone()));
            }
            let mut state = owner.state.lock().unwrap();
            if let Some(error) = cleanup_failure {
                state.cleanup_errors.push(error);
            }
            state.nodes.remove(&id);
            ready.send_replace(Some(outcome));
        });
    }
    async fn page(&self, id: &str, after: u64, limit: usize) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self
            .head(id)
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
        if through <= after {
            return Ok(Vec::new());
        }
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: id.into(),
        };
        Ok(self
            .store
            .read_page(&key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }
    fn head(&self, id: &str) -> Option<u64> {
        if let Some(ended) = self.state.lock().unwrap().ended.get(id) {
            return Some(ended.head);
        }
        self.owned_context(id)
            .map(|context| context.timeline().sequence())
    }
    async fn own_end(&self, branch: &str, head: u64) -> Result<Option<(u64, BranchEndData)>> {
        if head == 0 {
            return Ok(None);
        }
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: branch.into(),
        };
        let mut window = TAIL_EVENTS;
        loop {
            let from = head.saturating_sub(window.saturating_sub(1)).max(1);
            let records = self.store.read(&key, from..=head).await?;
            if let Some(found) = records
                .iter()
                .rev()
                .find_map(|record| match &record.event.kind {
                    EventKind::BranchEnd(data) => Some((record.event.sequence, data.clone())),
                    _ => None,
                })
            {
                return Ok(Some(found));
            }
            if from == 1 {
                return Ok(None);
            }
            window = window.saturating_mul(8);
        }
    }
    async fn creation_source(
        &self,
        parent: &str,
        child: &str,
        anchor: u64,
    ) -> Result<Option<BranchSource>> {
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: parent.into(),
        };
        Ok(self
            .store
            .read(&key, anchor..=anchor)
            .await?
            .into_iter()
            .find_map(|record| match &record.event.kind {
                EventKind::BranchCreate(data) if data.branch == child => Some(data.source.clone()),
                _ => None,
            }))
    }
    /// The stored head of an ended line, `None` for an open or unknown one.
    fn ended_head(&self, id: &str) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .ended
            .get(id)
            .map(|ended| ended.head)
    }
    /// A line's directory record, published or still opening; absent once a
    /// failed creation removed it.
    fn info(&self, id: &str) -> Option<TimelineInfo> {
        let state = self.state.lock().unwrap();
        state
            .nodes
            .get(id)
            .map(|node| node.info.clone())
            .or_else(|| state.ended.get(id).map(|ended| ended.info.clone()))
    }
    async fn end(&self, id: &str, source: BranchSource) -> Result<()> {
        let context = {
            let state = self.state.lock().unwrap();
            if state.closing {
                return Err(AgentError::Closed("TimelineTree".into()));
            }
            if id == "main" {
                return Err(AgentError::InvalidInput(
                    "`main` cannot end; closing the Session is not an end".into(),
                ));
            }
            if state.ended.contains_key(id) {
                return Err(AgentError::BranchEnded(id.into()));
            }
            let node = state
                .nodes
                .get(id)
                .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
            if !matches!(&*node.ready.borrow(), Some(Ok(_))) {
                return Err(AgentError::UnknownBranch(id.into()));
            }
            node.context
                .clone()
                .ok_or_else(|| AgentError::UnknownBranch(id.into()))?
        };
        let data = BranchEndData {
            source,
            ancestor: None,
        };
        context
            .timeline()
            .append_registered(EventKind::BranchEnd(data.clone()).into(), |_| {
                if context.ended() {
                    return Err(AgentError::BranchEnded(id.into()));
                }
                context.mark_ended();
                Ok(())
            })?;
        loop {
            let notified = self.end_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.ended.contains_key(id) || state.pending.contains_key(id) {
                    return Ok(());
                }
                if state.closing {
                    return Err(AgentError::Closed("TimelineTree".into()));
                }
            }
            tokio::select! {
                _ = notified => {}
                _ = self.cancel.cancelled() => {
                    return Err(AgentError::Closed("TimelineTree".into()));
                }
            }
        }
    }
    fn on_end(self: &Arc<Self>, id: &str, sequence: u64, data: BranchEndData) {
        if data.ancestor.is_some() {
            return;
        }
        let context = self
            .state
            .lock()
            .unwrap()
            .nodes
            .get(id)
            .and_then(|node| node.context.clone());
        let running = context
            .and_then(|context| context.branch().state().ok())
            .is_some_and(|status| status.running);
        if running && matches!(data.source, BranchSource::Tool { .. }) {
            self.state
                .lock()
                .unwrap()
                .pending
                .insert(id.into(), PendingEnd { sequence, data });
            self.end_notify.notify_waiters();
            return;
        }
        self.close_ended(id, sequence, data);
    }
    /// Closes a line whose end command is part of the turn that just ended.
    fn on_turn_end(self: &Arc<Self>, id: &str) {
        let pending = self.state.lock().unwrap().pending.remove(id);
        if let Some(pending) = pending {
            self.close_ended(id, pending.sequence, pending.data);
        }
    }
    fn close_ended(self: &Arc<Self>, id: &str, sequence: u64, data: BranchEndData) {
        let owner = self.clone();
        let id = id.to_owned();
        {
            let state = self.state.lock().unwrap();
            if state.closing || state.ended.contains_key(&id) {
                return;
            }
            self.builds.spawn(async move {
                if let Err(error) = owner.finish_end(&id, sequence, data).await {
                    owner.state.lock().unwrap().cleanup_errors.push(error);
                }
                owner.end_notify.notify_waiters();
            });
        }
    }
    /// The lines below `id`, deepest first.
    fn descendants(&self, id: &str) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let mut found = Vec::new();
        let mut pending = vec![(id.to_owned(), 0usize)];
        while let Some((parent, depth)) = pending.pop() {
            for node in state
                .nodes
                .values()
                .filter(|node| node.info.parent.as_deref() == Some(parent.as_str()))
            {
                found.push((node.info.id.clone(), depth + 1));
                pending.push((node.info.id.clone(), depth + 1));
            }
        }
        found.sort_by_key(|found| std::cmp::Reverse(found.1));
        found.into_iter().map(|(id, _)| id).collect()
    }
    async fn finish_end(&self, id: &str, end_event: u64, data: BranchEndData) -> Result<()> {
        let mut errors = Vec::new();
        for child in self.descendants(id) {
            if let Err(error) = self.end_descendant(&child, id, &data.source).await {
                errors.push(error);
            }
        }
        if let Err(error) = self.close_and_record(id, end_event, data).await {
            errors.push(error);
        }
        AgentError::aggregate(errors)
    }
    async fn end_descendant(&self, id: &str, ancestor: &str, source: &BranchSource) -> Result<()> {
        let ready = {
            let state = self.state.lock().unwrap();
            if state.ended.contains_key(id) {
                return Ok(());
            }
            let node = state
                .nodes
                .get(id)
                .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
            (!matches!(&*node.ready.borrow(), Some(Ok(_)))).then(|| node.ready.subscribe())
        };
        if let Some(ready) = ready {
            tokio::select! {
                result = Self::await_branch(ready) => {
                    if result.is_err() {
                        return Ok(());
                    }
                }
                _ = self.cancel.cancelled() => {
                    return Err(AgentError::Closed("TimelineTree".into()));
                }
            }
        }
        let context = {
            let state = self.state.lock().unwrap();
            if state.ended.contains_key(id) {
                return Ok(());
            }
            state
                .nodes
                .get(id)
                .and_then(|node| node.context.clone())
                .ok_or_else(|| AgentError::UnknownBranch(id.into()))?
        };
        let data = BranchEndData {
            source: source.clone(),
            ancestor: Some(ancestor.into()),
        };
        let sequence =
            context
                .timeline()
                .append_registered(EventKind::BranchEnd(data.clone()).into(), |_| {
                    if context.ended() {
                        return Err(AgentError::BranchEnded(id.into()));
                    }
                    context.mark_ended();
                    Ok(())
                });
        let sequence = match sequence {
            Ok(event) => event.sequence,
            Err(AgentError::BranchEnded(_)) => return self.wait_ended(id).await,
            Err(error) => return Err(error),
        };
        self.close_and_record(id, sequence, data).await
    }
    /// Waits until the line is recorded as ended.
    async fn wait_ended(&self, id: &str) -> Result<()> {
        loop {
            let notified = self.end_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let pending = {
                let mut state = self.state.lock().unwrap();
                if state.ended.contains_key(id) {
                    return Ok(());
                }
                if state.closing {
                    return Err(AgentError::Closed("TimelineTree".into()));
                }
                state.pending.remove(id)
            };
            if let Some(pending) = pending {
                return self
                    .close_and_record(id, pending.sequence, pending.data)
                    .await;
            }
            tokio::select! {
                _ = notified => {}
                _ = self.cancel.cancelled() => {
                    return Err(AgentError::Closed("TimelineTree".into()));
                }
            }
        }
    }
    async fn close_and_record(&self, id: &str, end_event: u64, data: BranchEndData) -> Result<()> {
        let (context, info) = {
            let state = self.state.lock().unwrap();
            let node = state
                .nodes
                .get(id)
                .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
            (
                node.context
                    .clone()
                    .ok_or_else(|| AgentError::UnknownBranch(id.into()))?,
                node.info.clone(),
            )
        };
        let parent = info.parent.clone();
        let mut errors = Vec::new();
        if let Err(error) = context.branch().close().await {
            errors.push(error);
        }
        let head = context.timeline().sequence();
        let recorded = parent.as_deref().map_or(Ok(()), |parent| {
            self.owned_context(parent)
                .ok_or_else(|| AgentError::UnknownBranch(parent.into()))
                .and_then(|parent| {
                    parent
                        .timeline()
                        .append(
                            EventKind::BranchEnded(BranchEndedData {
                                branch: id.into(),
                                end_event,
                                source: data.source,
                                ancestor: data.ancestor,
                            })
                            .into(),
                        )
                        .map(|_| ())
                })
        });
        {
            let mut state = self.state.lock().unwrap();
            state.pending.remove(id);
            state.nodes.remove(id);
            state.ended.insert(
                id.into(),
                EndedLine {
                    head,
                    info: info.clone(),
                },
            );
        }
        if let Err(error) = recorded {
            errors.push(AgentError::Storage(format!(
                "ending line {id}: its result could not be recorded: {error}"
            )));
        }
        AgentError::aggregate(errors)
    }
    fn on_create(&self, parent: &str, timeline: &Timeline, event: &AgentEvent) -> Result<()> {
        let EventKind::BranchCreate(data) = &event.kind else {
            unreachable!("branch subscription")
        };
        let point = match data.at {
            BranchCreateAt::Sequence { sequence } => sequence,
            BranchCreateAt::Create {} => event.sequence,
        };
        let mut state = self.state.lock().unwrap();
        if state.closing {
            return Ok(());
        }
        if !state
            .nodes
            .get(&data.branch)
            .is_some_and(|node| !node.started)
        {
            return Ok(());
        }
        let mut ancestors = timeline.ancestors();
        ancestors.push((parent.into(), point));
        self.launch(
            &mut state,
            data.branch.clone(),
            ancestors,
            Some((timeline.clone(), point)),
            Opening::Start {
                start: BranchStartData {
                    parent: Some(parent.into()),
                    create_event: Some(event.sequence),
                },
                first: Some(first_input(parent, data)),
            },
        );
        Ok(())
    }
    async fn await_branch(mut ready: watch::Receiver<Option<Result<Branch>>>) -> Result<Branch> {
        loop {
            if let Some(result) = ready.borrow().clone() {
                return result;
            }
            ready
                .changed()
                .await
                .map_err(|_| AgentError::fault("TimelineTree", "build result channel closed"))?;
        }
    }
    fn cancel_pending(
        state: &mut TreeState,
        parent: Option<&str>,
    ) -> Vec<(TimelineInfo, watch::Sender<Option<Result<Branch>>>)> {
        let mut cancelled = Vec::new();
        state.nodes.retain(|_, node| {
            if !node.started
                && parent.is_none_or(|parent| node.info.parent.as_deref() == Some(parent))
            {
                cancelled.push((node.info.clone(), node.ready.clone()));
                false
            } else {
                true
            }
        });
        cancelled
    }
    fn settle_cancelled(
        &self,
        cancelled: Vec<(TimelineInfo, watch::Sender<Option<Result<Branch>>>)>,
    ) {
        for (info, ready) in cancelled {
            let error = AgentError::Cancelled("branch creation closed before dispatch".into());
            let outcome = self.settle_failed(&info, error);
            ready.send_replace(Some(Err(outcome)));
        }
    }
    fn start_close(&self) {
        self.begin_close();
        if self.close_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = self.weak.upgrade().expect("owned close");
        let task = tokio::spawn(async move {
            owner.builds.close();
            owner.builds.wait().await;
            let (contexts, failures): (Vec<_>, Vec<_>) = {
                let mut state = owner.state.lock().unwrap();
                (
                    state
                        .nodes
                        .values()
                        .filter_map(|node| node.context.clone())
                        .collect(),
                    std::mem::take(&mut state.cleanup_errors),
                )
            };
            let mut errors = failures;
            errors.extend(
                join_all(contexts.iter().map(|context| context.close()))
                    .await
                    .into_iter()
                    .filter_map(|result| result.err()),
            );
            owner
                .close_result
                .send_replace(Some(AgentError::aggregate(errors)));
        });
        *self.close_task.lock().unwrap() = Some(task);
    }
}
impl TimelineTreeControl for TimelineTree {
    fn begin_close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closing = true;
        let cancelled = Self::cancel_pending(&mut state, None);
        drop(state);
        self.settle_cancelled(cancelled);
        self.cancel.cancel();
    }
    fn create_main(&self) -> BoxFuture<'_, Result<Branch>> {
        Box::pin(async move {
            let ready = {
                let mut state = self.state.lock().unwrap();
                let ready = self.reserve(
                    &mut state,
                    TimelineInfo {
                        id: "main".into(),
                        parent: None,
                        branch_point: None,
                        create_anchor: None,
                        source: None,
                    },
                )?;
                self.launch(
                    &mut state,
                    "main".into(),
                    vec![],
                    None,
                    Opening::Start {
                        start: BranchStartData {
                            parent: None,
                            create_event: None,
                        },
                        first: None,
                    },
                );
                ready
            };
            Self::await_branch(ready).await
        })
    }
    fn restore(&self, branches: Vec<StoredBranch>) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if branches.is_empty() {
                self.create_main().await?;
                return Ok(());
            }
            let mut remaining: BTreeMap<_, _> = branches
                .into_iter()
                .map(|branch| (branch.info.id.clone(), branch))
                .collect();
            let mut failed_creates = BTreeSet::new();
            let mut sources = BTreeMap::new();
            let mut recorded_ends = BTreeSet::new();
            let mut inactive = BTreeSet::new();
            while !remaining.is_empty() {
                let stored = remaining
                    .values()
                    .find(|branch| {
                        branch.info.parent.as_ref().is_none_or(|parent| {
                            self.get(parent).is_some() || inactive.contains(parent)
                        })
                    })
                    .cloned()
                    .ok_or_else(|| {
                        AgentError::Storage(
                            "branch directory contains missing parents or a cycle".into(),
                        )
                    })?;
                let mut next: TimelineInfo = stored.info.into();
                next.source = sources.get(&next.id).cloned();
                if recorded_ends.contains(&next.id) {
                    self.state.lock().unwrap().ended.insert(
                        next.id.clone(),
                        EndedLine {
                            head: stored.sequence,
                            info: next.clone(),
                        },
                    );
                    remaining.remove(&next.id);
                    inactive.insert(next.id);
                    continue;
                }
                if next.parent.as_ref().is_some_and(|parent| {
                    inactive.contains(parent)
                        || failed_creates.contains(&(
                            parent.clone(),
                            next.create_anchor.expect("validated creation anchor"),
                        ))
                }) {
                    let ended_parent = next
                        .parent
                        .as_deref()
                        .filter(|parent| self.ended_head(parent).is_some());
                    if let Some(parent) = ended_parent
                        && self.created(&next.id, stored.sequence).await?
                    {
                        if next.source.is_none()
                            && let Some(anchor) = next.create_anchor
                        {
                            next.source = self.creation_source(parent, &next.id, anchor).await?;
                        }
                        self.state.lock().unwrap().ended.insert(
                            next.id.clone(),
                            EndedLine {
                                head: stored.sequence,
                                info: next.clone(),
                            },
                        );
                    }
                    remaining.remove(&next.id);
                    inactive.insert(next.id);
                    continue;
                }
                let own_end = if next.parent.is_some() {
                    self.own_end(&next.id, stored.sequence).await?
                } else {
                    None
                };
                if let Some((end_event, data)) = own_end {
                    self.complete_stored_ends(&next, &mut remaining).await?;
                    let head = remaining[&next.id].sequence;
                    self.complete_end(&next, end_event, data)?;
                    self.state.lock().unwrap().ended.insert(
                        next.id.clone(),
                        EndedLine {
                            head,
                            info: next.clone(),
                        },
                    );
                    remaining.remove(&next.id);
                    inactive.insert(next.id);
                    continue;
                }
                if next.parent.is_some() && !self.created(&next.id, stored.sequence).await? {
                    let error =
                        AgentError::Build("branch creation interrupted by process restart".into());
                    self.record_create_failed(&next, &error)?;
                    remaining.remove(&next.id);
                    inactive.insert(next.id);
                    continue;
                }
                let ancestors = match &next.parent {
                    Some(parent) => {
                        let parent_context = self.owned_context(parent).expect("restored parent");
                        let timeline = parent_context.timeline();
                        let cut = next.branch_point.expect("validated child cut");
                        let context =
                            crate::history::read_context(timeline, cut)
                                .await
                                .map_err(|error| {
                                    AgentError::Storage(format!(
                                        "branch {} cannot be restored: {error}",
                                        next.id
                                    ))
                                })?;
                        crate::history::validate_context_boundary(&context)
                            .map_err(AgentError::Storage)?;
                        let mut ancestors = timeline.ancestors();
                        ancestors.push((parent.clone(), cut));
                        ancestors
                    }
                    None => vec![],
                };
                let (ready, reported) = {
                    let mut state = self.state.lock().unwrap();
                    let ready = self.reserve(&mut state, next.clone())?;
                    let (opening, reported) = if next.parent.is_none() && stored.sequence == 0 {
                        let start = BranchStartData {
                            parent: None,
                            create_event: None,
                        };
                        (Opening::Start { start, first: None }, None)
                    } else {
                        let (lines, reported) = oneshot::channel();
                        let head = stored.sequence;
                        (Opening::Resume { head, lines }, Some(reported))
                    };
                    self.launch(&mut state, next.id.clone(), ancestors, None, opening);
                    (ready, reported)
                };
                Self::await_branch(ready).await?;
                if let Some(reported) = reported {
                    let facts = reported
                        .await
                        .expect("a resumed branch reports its own lines before it is published");
                    failed_creates.extend(
                        facts
                            .failed_creates
                            .into_iter()
                            .map(|create| (next.id.clone(), create)),
                    );
                    sources.extend(facts.sources);
                    recorded_ends.extend(facts.ended);
                }
                remaining.remove(&next.id);
            }
            Ok(())
        })
    }
    fn branch(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: CancellationToken,
    ) -> BoxFuture<'_, Result<Branch>> {
        Box::pin(async move {
            if self.state.lock().unwrap().ended.contains_key(&parent) {
                return Err(AgentError::BranchEnded(parent.clone()));
            }
            let parent_context = self
                .owned_context(&parent)
                .ok_or_else(|| AgentError::UnknownBranch(parent.clone()))?;
            let timeline = parent_context.timeline();
            let parent_length = timeline.sequence();
            if at > parent_length {
                return Err(AgentError::InvalidInput(
                    "branch position is beyond committed history".into(),
                ));
            }
            let requested = crate::history::read_context(timeline, at).await?;
            crate::history::validate_context_boundary(&requested)
                .map_err(AgentError::InvalidInput)?;
            let original = BranchCreateData {
                at: if at == parent_length {
                    BranchCreateAt::Create {}
                } else {
                    BranchCreateAt::Sequence { sequence: at }
                },
                source: options.source,
                branch: uuid::Uuid::new_v4().to_string(),
                prompt: options.prompt,
            };
            if parent_context.is_closing() {
                return Err(AgentError::Closed(parent.clone()));
            }
            let mut ready = None;
            let _ =
                timeline.append_registered(EventKind::BranchCreate(original).into(), |event| {
                    let EventKind::BranchCreate(final_data) = &event.kind else {
                        unreachable!("before preserves kind")
                    };
                    let branch_point = match final_data.at {
                        BranchCreateAt::Create {} => event.sequence,
                        BranchCreateAt::Sequence { sequence } if sequence < event.sequence => {
                            sequence
                        }
                        BranchCreateAt::Sequence { .. } => {
                            return Err(AgentError::InvalidInput(
                                "branch position is beyond committed history".into(),
                            ));
                        }
                    };
                    if parent_context.is_closing() {
                        return Err(AgentError::Closed(parent.clone()));
                    }
                    if parent_context.ended() {
                        return Err(AgentError::BranchEnded(parent.clone()));
                    }
                    if admission.is_cancelled() {
                        return Err(AgentError::Cancelled("branch admission".into()));
                    }
                    let mut state = self.state.lock().unwrap();
                    let mut above = Some(parent.clone());
                    while let Some(id) = above {
                        if state.ended.contains_key(&id) {
                            return Err(AgentError::BranchEnded(id));
                        }
                        let node = state
                            .nodes
                            .get(&id)
                            .ok_or_else(|| AgentError::UnknownBranch(id.clone()))?;
                        if node.context.as_ref().is_some_and(|context| context.ended()) {
                            return Err(AgentError::BranchEnded(id));
                        }
                        above = node.info.parent.clone();
                    }
                    ready = Some(self.reserve(
                        &mut state,
                        TimelineInfo {
                            id: final_data.branch.clone(),
                            parent: Some(parent.clone()),
                            branch_point: Some(branch_point),
                            create_anchor: Some(event.sequence),
                            source: Some(final_data.source.clone()),
                        },
                    )?);
                    Ok(())
                })?;
            tokio::select! {
                result = Self::await_branch(ready.expect("committed branch reservation")) => result,
                _ = admission.cancelled() => Err(AgentError::Cancelled(
                    "stopped waiting; the branch creation continues".into(),
                )),
            }
        })
    }
    fn get(&self, id: &str) -> Option<Branch> {
        self.state.lock().unwrap().nodes.get(id).and_then(|node| {
            node.ready
                .borrow()
                .as_ref()
                .and_then(|result| result.as_ref().ok().cloned())
        })
    }
    fn end(&self, id: String, source: BranchSource) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { TimelineTree::end(self, &id, source).await })
    }
    fn ended_head(&self, id: &str) -> Option<u64> {
        TimelineTree::ended_head(self, id)
    }
    fn list(&self) -> Vec<TimelineInfo> {
        self.state
            .lock()
            .unwrap()
            .nodes
            .values()
            .filter(|node| matches!(&*node.ready.borrow(), Some(Ok(_))))
            .map(|node| node.info.clone())
            .collect()
    }
    fn close(&self) -> BoxFuture<'_, Result<()>> {
        self.start_close();
        Box::pin(async move {
            let mut receiver = self.close_result.subscribe();
            let result = loop {
                if let Some(result) = receiver.borrow().clone() {
                    break result;
                }
                receiver.changed().await.map_err(|_| {
                    AgentError::fault("TimelineTree", "close result channel closed")
                })?;
            };
            let task = self.close_task.lock().unwrap().take();
            if let Some(task) = task {
                task.await
                    .map_err(|error| AgentError::fault("TimelineTree close", error.to_string()))?;
            }
            result
        })
    }
}

/// Starts the tree's build task for each final `branch/create` of its branch.
pub struct TreeTimelinePlugin {
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    commands: Key<TreeCommands>,
    identity: Key<dim_agent_runtime::TimelineIdentity>,
}
impl TreeTimelinePlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            dispatcher: names.key()?,
            commands: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for TreeTimelinePlugin {
    fn name(&self) -> &str {
        "TreeTimeline"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.commands.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let commands = ctx.require_at(&self.commands)?;
            let identity = ctx.require_at(&self.identity)?;
            let cleanup = commands.clone();
            let parent = identity.branch_id.clone();
            ctx.effect(move || async move {
                if let Some(tree) = cleanup.tree.upgrade() {
                    let cancelled = TimelineTree::cancel_pending(
                        &mut tree.state.lock().unwrap(),
                        Some(&parent),
                    );
                    tree.settle_cancelled(cancelled);
                }
                Ok(())
            })?;
            dispatcher.subscribe(
                ctx,
                "TreeTimeline",
                &["branch/create", "branch/end", "turn/end"],
                move |event| {
                    let branch = &identity.branch_id;
                    let result = match &event.kind {
                        EventKind::BranchCreate(_) => commands
                            .tree
                            .upgrade()
                            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))
                            .and_then(|tree| tree.on_create(branch, &timeline, &event)),
                        EventKind::BranchEnd(data) => commands
                            .tree
                            .upgrade()
                            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))
                            .map(|tree| tree.on_end(branch, event.sequence, data.clone())),
                        EventKind::TurnEnd(_) => commands
                            .tree
                            .upgrade()
                            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))
                            .map(|tree| tree.on_turn_end(branch)),
                        _ => Ok(()),
                    };
                    Box::pin(async move { result })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
pub struct TimelineTreePlugin {
    commands: Key<TreeCommands>,
    tree: Key<TimelineTree>,
    control: Key<dyn TimelineTreeControl>,
    factory: Key<TimelineFactory>,
    store: Key<dyn TimelineStore>,
    identity: Key<SessionIdentity>,
    guide: Key<GuideEntries>,
}
impl TimelineTreePlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            commands: names.key()?,
            tree: names.key()?,
            control: names.key()?,
            factory: names.key()?,
            store: names.key()?,
            identity: names.key()?,
            guide: names.key()?,
        })
    }
}
impl DynPlugin for TimelineTreePlugin {
    fn name(&self) -> &str {
        "TimelineTree"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.factory.erase(),
            self.store.erase(),
            self.identity.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let factory = ctx.require_at(&self.factory)?;
            let store = ctx.require_at(&self.store)?;
            let identity = ctx.require_at(&self.identity)?;
            let guide = ctx.require_at(&self.guide)?;
            guide.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-branch-line".into()],
                    order: 40,
                    text: IDENTITY_ENTRY.into(),
                },
            )?;
            let tree = TimelineTree::new(factory, store, identity.session_id.clone());
            let owner = tree.clone();
            ctx.effect(move || async move { owner.close().await.map_err(Into::into) })?;
            let commands = Arc::new(TreeCommands {
                tree: Arc::downgrade(&tree),
            });
            ctx.provide_at(self.commands.clone(), commands)?;
            ctx.provide_at(self.tree.clone(), tree.clone())?;
            ctx.provide_at(self.control.clone(), tree as Arc<dyn TimelineTreeControl>)?;
            Ok(Effect::Done)
        })
    }
}
/// Session part: registers the `branch` tool, which forks the calling
/// branch through the tree.
pub struct BranchToolPlugin {
    tree: Key<TreeCommands>,
    tools: Key<ToolRegistry>,
    config: BranchToolConfig,
}
impl BranchToolPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Self::with_config(names, BranchToolConfig::default())
    }
    /// The plugin with a candidate's policy text at its factory
    /// configuration ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)).
    pub fn with_config(names: &Names<'_>, config: BranchToolConfig) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
            config,
        })
    }
}
impl DynPlugin for BranchToolPlugin {
    fn name(&self) -> &str {
        "BranchTool"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let tools = ctx.require_at(&self.tools)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: branch_definition(
                        self.config
                            .description
                            .as_deref()
                            .unwrap_or(BRANCH_TOOL_DESCRIPTION),
                    ),
                    handler: Arc::new(BranchTool { tree }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineRole {
    Main,
    Tool,
    Host,
}
/// One line its parent opened, as the parent's own facts recorded it.
struct Child {
    source: BranchSource,
    purpose: String,
    opened: u64,
    failed: bool,
    ended: bool,
}
enum Decision {
    Roster,
    Ended(String),
    Idle { turn: String, status: &'static str },
}
/// BranchLine's fold of its own line's facts.
#[derive(Default)]
struct LineView {
    role: Option<LineRole>,
    parent: Option<String>,
    /// The lines this line opened, open or not, by the order it opened them.
    children: BTreeMap<String, Child>,
    roster: Option<(String, bool)>,
    /// The turn that steered the parent, when one did.
    steered: Option<String>,
    steering: Option<(String, String)>,
    /// Inputs committed and not consumed, with their modes.
    held: BTreeMap<String, InputMode>,
    /// The sequence of each input's `input/applied`.
    applied: BTreeMap<String, u64>,
    /// The line committed its own `branch/end`: it is ending.
    ending: bool,
}
/// `<dim-branch-line>`, written by TimelineTree at the start of every child.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;

/// `<dim-branch-lines>`, the roster BranchLine writes on main.
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="main">: on main, the runtime lists the lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;

/// The roster's closing sentence, coordination text for `main`
/// ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)),
/// replaceable at [`BranchToolConfig::roster_closing`].
const ROSTER_CLOSING: &str = "They message you when they find something, need you, or end. Use the branch tool to message or read them, and wait to wait for them.";

/// `<dim-branch-message>`, `<dim-branch-idle>` and `<dim-branch-ended>`.
const BRANCH_MESSAGE_ENTRY: &str = r#"<dim-branch-message from="C" to="main">: a message from another line of this session, in that line's own words and not the operator's. dim-branch-idle and dim-branch-ended are the runtime's reports about a line you opened: information, not requests."#;

pub struct BranchLinePlugin {
    line: Key<BranchLine>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
    lifecycle: Key<BranchLifecycle>,
    tree: Key<TreeCommands>,
    guide: Key<GuideEntries>,
    config: BranchToolConfig,
}
impl BranchLinePlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Self::with_config(names, BranchToolConfig::default())
    }
    /// The plugin with a candidate's policy text at its factory
    /// configuration ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)).
    pub fn with_config(names: &Names<'_>, config: BranchToolConfig) -> rutis::Result<Self> {
        Ok(Self {
            line: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
            lifecycle: names.key()?,
            tree: names.key()?,
            guide: names.key()?,
            config,
        })
    }
}
impl DynPlugin for BranchLinePlugin {
    fn name(&self) -> &str {
        "BranchLine"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.identity.erase(),
            self.lifecycle.erase(),
            self.tree.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let tree = ctx.require_at(&self.tree)?;
            let guide = ctx.require_at(&self.guide)?;
            let scope = GuideScope::Branch(identity.branch_id.clone());
            for (markers, order, text) in [
                (vec!["dim-branch-lines"], 40, ROSTER_ENTRY),
                (
                    vec!["dim-branch-message", "dim-branch-idle", "dim-branch-ended"],
                    50,
                    BRANCH_MESSAGE_ENTRY,
                ),
            ] {
                guide.register(
                    ctx,
                    scope.clone(),
                    GuideEntry {
                        markers: markers.into_iter().map(str::to_owned).collect(),
                        order,
                        text: text.into(),
                    },
                )?;
            }
            let line = Arc::new(BranchLine {
                own: identity.branch_id.clone(),
                tree,
                roster_closing: self.config.roster_closing.clone(),
                view: Mutex::new(LineView::default()),
                wanted: Mutex::new(std::collections::VecDeque::new()),
                changed: tokio::sync::Notify::new(),
                stop: CancellationToken::new(),
            });
            let consumer = line.clone();
            dispatcher
                .follow(
                    ctx,
                    "BranchLine",
                    &[
                        "branch/create",
                        "branch/create-failed",
                        "branch/ended",
                        "branch/end",
                        "input/message",
                        "input/applied",
                        "input/consumed",
                        "input/deleted",
                        "stop/interrupt",
                        "turn/end",
                        "step/tool-use",
                        "step/tool-result",
                    ],
                    move |delivery| {
                        let consumer = consumer.clone();
                        Box::pin(async move {
                            match delivery {
                                Delivery::Replay(events) => consumer.rebuild(&events),
                                Delivery::Live(event) => consumer.consume(&event),
                            }
                            Ok(())
                        })
                    },
                )
                .await?;
            line.want(Decision::Roster);
            let writer = line.clone();
            let phase = lifecycle.clone();
            let tasks = TaskTracker::new();
            tasks.spawn(async move {
                tokio::select! {
                    _ = writer.stop.cancelled() => return,
                    active = phase.activated() => if !active { return },
                }
                writer.run().await;
            });
            let stop = line.stop.clone();
            ctx.effect(move || async move {
                stop.cancel();
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            ctx.provide_at(self.line.clone(), line)?;
            Ok(Effect::Done)
        })
    }
}
pub struct BranchLine {
    own: String,
    tree: Arc<TreeCommands>,
    /// The roster's closing sentence in place of [`ROSTER_CLOSING`].
    roster_closing: Option<Arc<str>>,
    view: Mutex<LineView>,
    wanted: Mutex<std::collections::VecDeque<Decision>>,
    changed: tokio::sync::Notify,
    stop: CancellationToken,
}
impl BranchLine {
    /// Rebuilds the fold from the line's own history, without writing.
    fn rebuild(&self, events: &[Arc<AgentEvent>]) {
        let _ = self.resolved();
        let mut view = self.view.lock().unwrap();
        for event in events {
            Self::fold(&mut view, event);
        }
    }
    /// Folds one live committed event and queues its decision.
    fn consume(&self, event: &AgentEvent) {
        let _ = self.resolved();
        let mut view = self.view.lock().unwrap();
        if let Some(decision) = Self::fold(&mut view, event) {
            drop(view);
            self.want(decision);
        }
    }
    fn fold(view: &mut LineView, event: &AgentEvent) -> Option<Decision> {
        match &event.kind {
            EventKind::BranchCreate(data) => {
                view.children.insert(
                    data.branch.clone(),
                    Child {
                        source: data.source.clone(),
                        purpose: data.prompt.clone(),
                        opened: event.sequence,
                        failed: false,
                        ended: false,
                    },
                );
                Some(Decision::Roster)
            }
            EventKind::BranchCreateFailed(data) => {
                let branch = match data {
                    BranchCreateFailedData::Failed { branch, .. }
                    | BranchCreateFailedData::Cancelled { branch, .. } => branch,
                };
                if let Some(child) = view.children.get_mut(branch) {
                    child.failed = true;
                }
                Some(Decision::Roster)
            }
            EventKind::BranchEnded(data) => {
                let child = view.children.get_mut(&data.branch)?;
                child.ended = true;
                let itself =
                    matches!(data.source, BranchSource::Tool { .. }) && data.ancestor.is_none();
                Some(
                    if !matches!(child.source, BranchSource::Tool { .. }) || itself {
                        Decision::Roster
                    } else {
                        Decision::Ended(data.branch.clone())
                    },
                )
            }
            EventKind::BranchEnd(_) => {
                view.ending = true;
                None
            }
            EventKind::InputMessage(data) => {
                view.held.insert(event.id.clone(), data.effective_mode());
                for block in &data.blocks {
                    if let InputBlock::Raw {
                        text,
                        key: Some(key),
                        retire,
                    } = block
                        && key == &line_key()
                    {
                        view.roster = Some((text.clone(), *retire));
                    }
                }
                None
            }
            EventKind::InputApplied(data) => {
                view.applied.insert(data.input_id.clone(), event.sequence);
                None
            }
            EventKind::StopInterrupt(_) => {
                let applied = &view.applied;
                view.held
                    .retain(|id, _| applied.get(id).is_none_or(|at| *at > event.sequence));
                None
            }
            EventKind::InputConsumed(data) => {
                view.held.remove(&data.input_id);
                view.applied.remove(&data.input_id);
                None
            }
            EventKind::InputDeleted(data) => {
                view.held.remove(&data.input_id);
                view.applied.remove(&data.input_id);
                None
            }
            EventKind::ToolUse(data) => {
                if let Some(parent) = view.parent.clone()
                    && steered_parent(&data.call, &parent)
                {
                    view.steering = Some((data.turn_id.clone(), data.call.id.clone()));
                }
                None
            }
            EventKind::ToolResult(data) => {
                if let Some((turn, call)) = view.steering.clone()
                    && let Some(result) = data.results.iter().find(|at| at.call_id == call)
                {
                    if result.result.is_error != Some(true) {
                        view.steered = Some(turn);
                    }
                    view.steering = None;
                }
                None
            }
            EventKind::TurnEnd(data) => {
                if view.held.values().any(|mode| *mode != InputMode::Append) {
                    return None;
                }
                let status = match &data.outcome {
                    TurnOutcome::Completed { .. } => "completed",
                    TurnOutcome::Failed { .. } => "failed",
                    TurnOutcome::Cancelled { .. } => "cancelled",
                };
                Some(Decision::Idle {
                    turn: data.turn_id.clone(),
                    status,
                })
            }
            _ => None,
        }
    }
    /// The line committed its own `branch/end`.
    fn ending(&self) -> bool {
        self.view.lock().unwrap().ending
    }
    /// The line's role and parent, from its directory record.
    fn resolved(&self) -> Option<(LineRole, Option<String>)> {
        {
            let view = self.view.lock().unwrap();
            if let Some(role) = view.role {
                return Some((role, view.parent.clone()));
            }
        }
        let info = self.tree.info(self.own.clone()).ok()?;
        let role = match info.source {
            None => LineRole::Main,
            Some(BranchSource::Tool { .. }) => LineRole::Tool,
            Some(BranchSource::Host { .. }) => LineRole::Host,
        };
        let mut view = self.view.lock().unwrap();
        view.role = Some(role);
        view.parent = info.parent.clone();
        Some((role, info.parent))
    }
    fn want(&self, decision: Decision) {
        self.wanted.lock().unwrap().push_back(decision);
        self.changed.notify_one();
    }
    async fn run(self: Arc<Self>) {
        loop {
            let next = self.wanted.lock().unwrap().pop_front();
            let Some(decision) = next else {
                tokio::select! {
                    _ = self.stop.cancelled() => return,
                    _ = self.changed.notified() => continue,
                }
            };
            if self.stop.is_cancelled() {
                return;
            }
            let _ = self.write(decision).await;
        }
    }
    async fn write(&self, decision: Decision) -> Result<()> {
        let Some((role, parent)) = self.resolved() else {
            return Ok(());
        };
        match decision {
            Decision::Roster => {
                if role != LineRole::Main {
                    return Ok(());
                }
                let (text, retired) = self.roster();
                let observed = self.view.lock().unwrap().roster.clone();
                let settled = observed.as_ref() == Some(&(text.clone(), retired))
                    || (retired && observed.as_ref().is_none_or(|(_, retired)| *retired));
                if settled {
                    return Ok(());
                }
                self.write_roster(text, retired, None).await
            }
            Decision::Ended(branch) => {
                if role != LineRole::Main {
                    return Ok(());
                }
                let (text, retired) = self.roster();
                self.write_roster(text, retired, Some(branch)).await
            }
            Decision::Idle { turn, status } => {
                let Some(parent) = parent.filter(|_| role == LineRole::Tool) else {
                    return Ok(());
                };
                {
                    let view = self.view.lock().unwrap();
                    if view.ending || view.steered.as_deref() == Some(turn.as_str()) {
                        return Ok(());
                    }
                }
                let block = crate::context_block::raw(
                    crate::context_block::element(
                        "dim-branch-idle",
                        &[("branch", &self.own), ("status", status)],
                        &format!(
                            "Line {}'s turn ended ({status}) without a message to you, and {} is waiting. Read it if you need its state; message it with mode \"steer\" to give it more work, or leave it.",
                            self.own, self.own
                        ),
                    ),
                    None,
                );
                let data = InputMessageData {
                    blocks: vec![block],
                    mode: Some(InputMode::Steer),
                    ..InputMessageData::default()
                };
                self.tree.input(parent, data).await.map(|_| ())
            }
        }
    }
    /// The roster the fold calls for and whether it retires the key.
    fn roster(&self) -> (String, bool) {
        let view = self.view.lock().unwrap();
        let mut open: Vec<(&String, &Child)> = view
            .children
            .iter()
            .filter(|(_, child)| {
                !child.failed && !child.ended && matches!(child.source, BranchSource::Tool { .. })
            })
            .collect();
        open.sort_by_key(|(_, child)| child.opened);
        if open.is_empty() {
            return (
                crate::context_block::element(
                    "dim-branch-lines",
                    &[("branch", &self.own)],
                    "All lines you opened have ended.",
                ),
                true,
            );
        }
        let listed = open
            .iter()
            .map(|(id, child)| {
                format!(
                    "- {id}: {}",
                    crate::context_block::text(&first_line(&child.purpose))
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        (
            crate::context_block::element(
                "dim-branch-lines",
                &[("branch", &self.own)],
                &format!(
                    "Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it does not see your work after that point unless you message it.\n{listed}\n{}",
                    self.roster_closing.as_deref().unwrap_or(ROSTER_CLOSING)
                ),
            ),
            false,
        )
    }
    async fn write_roster(
        &self,
        text: String,
        retired: bool,
        notice: Option<String>,
    ) -> Result<()> {
        let key = line_key();
        let block = if retired {
            crate::context_block::retire(text.clone(), key)
        } else {
            crate::context_block::raw(text.clone(), Some(key))
        };
        let mut blocks = vec![block];
        if let Some(branch) = &notice {
            blocks.push(crate::context_block::raw(
                crate::context_block::element(
                    "dim-branch-ended",
                    &[("branch", branch)],
                    &format!("Line {branch} was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did."),
                ),
                None,
            ));
        }
        let data = InputMessageData {
            blocks,
            mode: Some(if notice.is_some() {
                InputMode::Steer
            } else {
                InputMode::Append
            }),
            ..InputMessageData::default()
        };
        self.tree.input(self.own.clone(), data).await?;
        self.view.lock().unwrap().roster = Some((text, retired));
        Ok(())
    }
}
fn steered_parent(call: &ToolCall, parent: &str) -> bool {
    call.name == "branch"
        && call.input.get("action").and_then(|value| value.as_str()) == Some("message")
        && call.input.get("mode").and_then(|value| value.as_str()) == Some("steer")
        && call.input.get("to").and_then(|value| value.as_str()) == Some(parent)
}
/// The first line of a purpose, at most 120 characters (design §12.1).
fn first_line(purpose: &str) -> String {
    purpose
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

struct BranchTool {
    tree: Arc<TreeCommands>,
}
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Target events per `read` page; the final atomic batch may exceed it.
const READ_PAGE_LIMIT: usize = 200;
/// The `branch` tool's action-based input (design §11).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchInput {
    action: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    at: Option<u64>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    mode: Option<InputMode>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    after: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}
fn refusal(code: &str, text: String) -> ToolResult {
    let mut output = ToolResult::error(text.clone());
    output.structured_content = serde_json::json!({"error": code, "message": text})
        .as_object()
        .cloned();
    output
}
fn invalid(reason: impl std::fmt::Display) -> HostError {
    HostError::failed(format!("Invalid branch arguments: {reason}"))
}
impl BranchTool {
    async fn run(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> std::result::Result<ToolResult, HostError> {
        let input: BranchInput =
            serde_json::from_value(serde_json::Value::Object(call.input)).map_err(invalid)?;
        for value in [input.at, input.after] {
            if value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
                return Err(invalid("positions must be safe integers"));
            }
        }
        let branch = self
            .tree
            .get(&context.branch_id)
            .map_err(|error| HostError::failed(error.to_string()))?;
        let line = branch
            .service::<BranchLine>()
            .map_err(|error| HostError::failed(error.to_string()))?;
        let own = match line.resolved() {
            Some((role, parent)) => OwnLine {
                id: context.branch_id.clone(),
                role,
                parent,
                ending: line.ending(),
            },
            None => {
                return Err(HostError::failed(
                    "the calling line has no directory record",
                ));
            }
        };
        crate::context_policy::check_cancel(&cancel)?;
        match input.action.as_str() {
            "create" => self.create(&own, input, &context, &call.id, cancel).await,
            "message" => self.message(&own, input).await,
            "read" => self.read(&own, input).await,
            "end" => tokio::select! {
                result = self.end(&own, input, &context, &call.id) => result,
                _ = cancel.cancelled() => Err(HostError::Cancelled("branch end cancelled".into())),
            },
            action => Err(invalid(format!("unknown action {action}"))),
        }
    }
    /// Opens a tool line cut before this request's model start (design §11).
    async fn create(
        &self,
        own: &OwnLine,
        input: BranchInput,
        context: &ToolExecutionContext,
        call_id: &str,
        cancel: CancellationToken,
    ) -> std::result::Result<ToolResult, HostError> {
        if own.role != LineRole::Main {
            return Ok(refusal(
                "branch_not_main",
                "Only main opens lines. Tell main if more parallel work is needed.".into(),
            ));
        }
        let prompt = input.prompt.unwrap_or_default();
        if prompt.is_empty() {
            return Err(invalid("create requires a nonempty prompt"));
        }
        let at = match input.at {
            Some(at) => at,
            None => {
                let parent = self
                    .tree
                    .get(&context.branch_id)
                    .map_err(|error| HostError::failed(error.to_string()))?;
                let events = parent
                    .local(..)
                    .await
                    .map_err(|error| HostError::failed(error.to_string()))?;
                let first_use = events
                    .iter()
                    .position(|event| {
                        matches!(&event.kind, EventKind::ToolUse(data)
                            if data.turn_id == context.turn_id
                                && data.request_id == context.request_id)
                    })
                    .ok_or_else(|| {
                        HostError::failed("branch tool call is absent from its owning timeline")
                    })?;
                events[..first_use]
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        EventKind::ModelStart(data)
                            if data.turn_id == context.turn_id
                                && data.request_id == context.request_id =>
                        {
                            Some(event.sequence - 1)
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        HostError::failed("branch tool batch has no owning model start")
                    })?
            }
        };
        let branch = self
            .tree
            .branch(
                context.branch_id.clone(),
                at,
                BranchOptions {
                    prompt,
                    source: BranchSource::Tool {
                        turn_id: context.turn_id.clone(),
                        request_id: context.request_id.clone(),
                        call_id: call_id.into(),
                    },
                },
                cancel,
            )
            .await
            .map_err(|error| match error {
                AgentError::Cancelled(reason) => HostError::Cancelled(reason),
                error => HostError::failed(error.to_string()),
            })?;
        Ok(ToolResult::text(format!("Line created: {}", branch.id())))
    }
    /// Sends one message to a line of the calling line's pair (design §6).
    async fn message(
        &self,
        own: &OwnLine,
        input: BranchInput,
    ) -> std::result::Result<ToolResult, HostError> {
        let to = input
            .to
            .ok_or_else(|| invalid("message requires to"))?
            .clone();
        let content = input
            .content
            .ok_or_else(|| invalid("message requires content"))?;
        let mode = match input.mode.unwrap_or(InputMode::Append) {
            InputMode::Queue => return Err(invalid("Queue is not a branch message mode")),
            mode => mode,
        };
        let target = self.line(&to);
        let Some(target) = target else {
            return Ok(self.not_kin());
        };
        if !kin(own, &target) {
            return Ok(self.not_kin());
        }
        if matches!(target.source, Some(BranchSource::Host { .. })) {
            return Ok(self.not_tool_line(&target.id));
        }
        let data = InputMessageData {
            blocks: vec![message_block(&own.id, &target.id, &content, false)],
            mode: Some(mode),
            ..InputMessageData::default()
        };
        match self.tree.input(target.id.clone(), data).await {
            Ok(_) => Ok(ToolResult::text(format!("Sent to {}.", target.id))),
            Err(AgentError::BranchEnded(line)) => Ok(self.ended(&line)),
            Err(error) => Err(HostError::failed(error.to_string())),
        }
    }
    /// One page of a tool line's committed history, open or ended (design §6).
    async fn read(
        &self,
        own: &OwnLine,
        input: BranchInput,
    ) -> std::result::Result<ToolResult, HostError> {
        let id = input
            .branch
            .ok_or_else(|| invalid("read requires branch"))?
            .clone();
        let Some(target) = self.line(&id) else {
            return Ok(self.not_kin());
        };
        if own.role != LineRole::Main || !kin(own, &target) {
            return Ok(self.not_kin());
        }
        if matches!(target.source, Some(BranchSource::Host { .. })) {
            return Ok(self.not_tool_line(&target.id));
        }
        let limit = match input.limit.unwrap_or(READ_PAGE_LIMIT as u64) {
            limit if (1..=READ_PAGE_LIMIT as u64).contains(&limit) => limit as usize,
            _ => {
                return Err(invalid(format!(
                    "limit must be between 1 and {READ_PAGE_LIMIT}"
                )));
            }
        };
        let after = input.after.unwrap_or(0);
        let events = self
            .tree
            .page(target.id.clone(), after, limit)
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let head = self
            .tree
            .head(&target.id)
            .ok_or_else(|| HostError::failed(format!("unknown line {}", target.id)))?;
        let (items, next) = read_items(&events, after);
        let done = next >= head;
        let page = serde_json::json!({"items": items, "nextCursor": next, "done": done});
        let mut output = ToolResult::text(page.to_string());
        output.structured_content = page.as_object().cloned();
        Ok(output)
    }
    async fn end(
        &self,
        own: &OwnLine,
        input: BranchInput,
        context: &ToolExecutionContext,
        call_id: &str,
    ) -> std::result::Result<ToolResult, HostError> {
        if own.role != LineRole::Tool {
            return Ok(refusal(
                "branch_cannot_end",
                "Only a line opened with create can end itself.".into(),
            ));
        }
        if own.ending {
            return Ok(self.ended(&own.id));
        }
        let content = input
            .content
            .ok_or_else(|| invalid("end requires content"))?;
        let parent = own
            .parent
            .clone()
            .ok_or_else(|| invalid("a tool line has a parent"))?;
        let data = InputMessageData {
            blocks: vec![message_block(&own.id, &parent, &content, true)],
            mode: Some(InputMode::Steer),
            ..InputMessageData::default()
        };
        if let Err(error) = self.tree.input(parent.clone(), data).await {
            return Err(HostError::failed(error.to_string()));
        }
        let source = BranchSource::Tool {
            turn_id: context.turn_id.clone(),
            request_id: context.request_id.clone(),
            call_id: call_id.into(),
        };
        match self.tree.end(own.id.clone(), source).await {
            Ok(()) => Ok(ToolResult::text(format!(
                "Line {} is ending; your final message was sent to {}.",
                own.id, parent
            ))),
            Err(AgentError::BranchEnded(line)) => Ok(self.ended(&line)),
            Err(error) => Err(HostError::failed(error.to_string())),
        }
    }
    fn line(&self, id: &str) -> Option<TimelineInfo> {
        self.tree.info(id.to_string()).ok()
    }
    fn not_kin(&self) -> ToolResult {
        refusal(
            "branch_not_kin",
            "You can message only the lines you opened and the line that opened you.".into(),
        )
    }
    fn not_tool_line(&self, id: &str) -> ToolResult {
        refusal(
            "branch_not_tool_line",
            format!("Line {id} was not opened with this tool; it follows its own protocol."),
        )
    }
    fn ended(&self, id: &str) -> ToolResult {
        refusal(
            "branch_ended",
            format!("Line {id} has ended. Read it to see what it did."),
        )
    }
}
/// The calling line, as its own BranchLine sees it (design §5, §6).
struct OwnLine {
    id: String,
    role: LineRole,
    parent: Option<String>,
    /// The line committed its own `branch/end`: it is ending.
    ending: bool,
}
fn kin(own: &OwnLine, target: &TimelineInfo) -> bool {
    match own.role {
        LineRole::Tool => target.id == own.parent.clone().unwrap_or_default(),
        LineRole::Main => target.parent.as_deref() == Some(own.id.as_str()),
        LineRole::Host => false,
    }
}
fn message_block(from: &str, to: &str, content: &str, final_message: bool) -> InputBlock {
    let content = crate::context_block::text(content);
    if final_message {
        crate::context_block::raw(
            crate::context_block::element(
                "dim-branch-message",
                &[("from", from), ("to", to), ("final", "true")],
                &format!("{content}\n\nLine {from} ends with this message."),
            ),
            None,
        )
    } else {
        crate::context_block::raw(
            crate::context_block::element(
                "dim-branch-message",
                &[("from", from), ("to", to)],
                &content,
            ),
            None,
        )
    }
}
fn read_items(events: &[Arc<AgentEvent>], after: u64) -> (Vec<serde_json::Value>, u64) {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut next = after;
    let mut text_request: Option<String> = None;
    let mut names = BTreeMap::<(String, String), String>::new();
    for event in events {
        next = event.sequence;
        let mut merged = None;
        match &event.kind {
            EventKind::InputMessage(data) => items.push(serde_json::json!({
                "sequence": event.sequence, "type": "input",
                "mode": data.effective_mode(), "user": data.user, "blocks": data.blocks})),
            EventKind::Text(data) => {
                if text_request.as_deref() == Some(data.request_id.as_str())
                    && let Some(text) = items.last_mut().and_then(|item| item.get_mut("text"))
                {
                    *text =
                        serde_json::json!(format!("{}{}", text.as_str().unwrap_or(""), data.delta));
                } else {
                    items.push(serde_json::json!({
                        "sequence": event.sequence, "type": "text", "text": data.delta}));
                }
                merged = Some(data.request_id.clone());
            }
            EventKind::ToolUseStart(data) => {
                names.insert(
                    (data.request_id.clone(), data.call_id.clone()),
                    data.name.clone(),
                );
            }
            EventKind::ToolCall(data) => {
                for call in &data.calls {
                    names.insert(
                        (data.request_id.clone(), call.id.clone()),
                        call.name.clone(),
                    );
                    items.push(serde_json::json!({
                        "sequence": event.sequence, "type": "tool_call",
                        "callId": call.id, "name": call.name}));
                }
            }
            EventKind::ToolResult(data) => {
                for result in &data.results {
                    let key = (data.request_id.clone(), result.call_id.clone());
                    let name = names.get(&key);
                    let mut item = serde_json::json!({
                        "sequence": event.sequence, "type": "tool_result",
                        "callId": result.call_id,
                        "status": if result.result.is_error == Some(true) { "error" } else { "ok" }});
                    if let Some(name) = name {
                        item["name"] = serde_json::json!(name);
                    }
                    items.push(item);
                }
            }
            EventKind::TurnEnd(data) => {
                let mut item = serde_json::json!({"sequence": event.sequence, "type": "turn_end"});
                match &data.outcome {
                    TurnOutcome::Completed { .. } => {
                        item["status"] = serde_json::json!("completed")
                    }
                    TurnOutcome::Failed { error } => {
                        item["status"] = serde_json::json!("failed");
                        item["reason"] = serde_json::json!(error);
                    }
                    TurnOutcome::Cancelled { reason } => {
                        item["status"] = serde_json::json!("cancelled");
                        item["reason"] = serde_json::json!(reason);
                    }
                }
                items.push(item);
            }
            _ => {}
        }
        text_request = merged;
    }
    (items, next)
}
impl ToolHandler for BranchTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move { self.run(call, context, cancel).await })
    }
}
fn line_key() -> String {
    "branch:line".into()
}

fn first_input(parent: &str, data: &BranchCreateData) -> InputMessageData {
    let (blocks, user) = match data.source {
        BranchSource::Tool { .. } => (
            vec![tool_line_identity(&data.branch, parent, &data.prompt)],
            None,
        ),
        BranchSource::Host { .. } => (
            vec![host_line_identity(&data.branch, parent)],
            Some(UserInput {
                content: vec![ContentBlock::Text {
                    text: data.prompt.clone(),
                }],
            }),
        ),
    };
    InputMessageData {
        blocks,
        user,
        mode: Some(InputMode::Queue),
        ..InputMessageData::default()
    }
}

fn tool_line_identity(line: &str, parent: &str, prompt: &str) -> InputBlock {
    let prompt = crate::context_block::text(prompt);
    let body = [
        format!("You are line {line} of this session. Line {parent} opened you at this point of its history to work in parallel with it. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line. {parent} keeps working on its line; you do not see its work after this point, and it sees yours only through what you send it. Messages above that were addressed to {parent}, such as its list of open lines, are {parent}'s, not yours."),
        String::new(),
        format!("Your purpose, as {parent} wrote it:"),
        prompt,
        String::new(),
        format!("How you work with {parent}:"),
        format!("- Work on this purpose only. You cannot open lines; if more parallel work is needed, tell {parent}."),
        format!("- Tell {parent} with the branch tool (message, to \"{parent}\"). Use mode \"steer\" for what {parent} must act on now: a finding that changes its work or another line's, a blocking question, a conflict. Use \"append\" for progress {parent} should see at its next step. Do not repeat what {parent} already knows: when several steps give the same result, send one message for the run of them, and say only what changes what {parent} does or what it cannot see for itself."),
        format!("- You share the workspace with {parent} and its other lines. Change only the files {parent} gave you; before changing any other file, tell {parent} which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell {parent} before you continue."),
        "- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.".to_string(),
        format!("- When your purpose is done, or cannot be done, call the branch tool with action \"end\" and content saying what you found, did, changed and left undone. If you already told {parent} that in a message, your end content carries only what is new. That is your last action; {parent} receives it."),
    ]
    .join("\n");
    crate::context_block::raw(
        crate::context_block::element(
            "dim-branch-line",
            &[("branch", line), ("parent", parent), ("source", "tool")],
            &body,
        ),
        Some(line_key()),
    )
}

fn host_line_identity(line: &str, parent: &str) -> InputBlock {
    crate::context_block::raw(
        crate::context_block::element(
            "dim-branch-line",
            &[("branch", line), ("parent", parent), ("source", "host")],
            &format!(
                "You are line {line} of this session, opened from line {parent} at this point of its history. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line, in parallel with {parent}. You do not see {parent}'s work after this point. Messages above that were addressed to {parent} are {parent}'s, not yours. The message after this one says what this line is for."
            ),
        ),
        Some(line_key()),
    )
}

/// The `branch` tool's description: its policy sentences are the coordination
/// text for `main` ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)),
/// replaceable at [`BranchToolConfig`].
const BRANCH_TOOL_DESCRIPTION: &str = "Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed while you work.\n- create (main only): open a line to work on prompt in parallel. Open one line per part that can go on at the same time as the rest — separate files, a separate question, or a second approach — instead of doing those parts one after another yourself; not for a single quick edit, nor when your next step needs its result. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Returns the line's id.\n- message: send content to a line you opened, or to the line that opened you. Mode \"append\" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use \"steer\" when the receiver must act now or is idle.\n- read: show the recent activity of a line you opened, also after it ended. Pass nextCursor as after to continue.\n- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.\nLines talk only with the line that opened them and the lines they opened.";

/// Factory configuration of the branch tool ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)).
/// The product sets none of it, so the built-in text is in force.
#[derive(Clone, Default)]
pub struct BranchToolConfig {
    /// The tool description in place of [`BRANCH_TOOL_DESCRIPTION`]. A
    /// candidate keeps the description's protocol sentences byte-identical
    /// ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)).
    pub description: Option<Arc<str>>,
    /// The roster's closing sentence in place of [`ROSTER_CLOSING`], read by
    /// BranchLine, the part that writes the roster
    /// ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)).
    pub roster_closing: Option<Arc<str>>,
}

fn branch_definition(description: &str) -> ToolDefinition {
    ToolDefinition {
        risk: Some("session".into()),
        ..crate::tool_registry::definition(
            "branch",
            description,
            serde_json::json!({"type":"object","required":["action"],"additionalProperties":false,"properties":{
                "action":{"type":"string","enum":["create","message","read","end"],"description":"What the call does."},
                "prompt":{"type":"string","description":"create: the purpose of the new line."},
                "at":{"type":"number","minimum":0,"description":"create: the position to cut at; defaults to before this request."},
                "to":{"type":"string","description":"message: the line to send to."},
                "content":{"type":"string","description":"message: what to send. end: your final message."},
                "mode":{"type":"string","enum":["append","steer"],"description":"message: append (default) or steer."},
                "branch":{"type":"string","description":"read: the line to read."},
                "after":{"type":"number","minimum":0,"description":"read: the cursor from nextCursor."},
                "limit":{"type":"number","minimum":1,"maximum":200,"description":"read: target events per page."}
            }}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(sequence: u64, kind: EventKind) -> Arc<AgentEvent> {
        Arc::new(AgentEvent {
            id: format!("event-{sequence}"),
            sequence,
            create_at: 1_750_000_000_000 + sequence,
            tags: None,
            kind,
        })
    }
    fn call(sequence: u64, id: &str, name: &str, input: serde_json::Value) -> Arc<AgentEvent> {
        event(
            sequence,
            EventKind::ToolUse(ToolUseData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call: ToolCall {
                    id: id.into(),
                    name: name.into(),
                    input: input.as_object().cloned().unwrap_or_default(),
                },
            }),
        )
    }
    fn result(sequence: u64, id: &str, is_error: bool) -> Arc<AgentEvent> {
        event(
            sequence,
            EventKind::ToolResult(ToolResultData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_event_id: Some("call-event".into()),
                results: vec![ToolCallResult {
                    call_id: id.into(),
                    result: if is_error {
                        ToolResult::error("refused")
                    } else {
                        ToolResult::text("Sent to main.")
                    },
                }],
            }),
        )
    }
    /// A tool line's view, as its own history left it.
    fn line() -> LineView {
        LineView {
            role: Some(LineRole::Tool),
            parent: Some("main".into()),
            ..LineView::default()
        }
    }

    #[test]
    fn only_a_delivered_steer_silences_the_turn_end() {
        let steer = json!({"action":"message","to":"main","mode":"steer","content":"done"});
        let mut refused = line();
        BranchLine::fold(&mut refused, &call(1, "call", "branch", steer.clone()));
        assert_eq!(refused.steering, Some(("turn".into(), "call".into())));
        BranchLine::fold(&mut refused, &result(2, "call", true));
        assert_eq!(
            refused.steered, None,
            "a refused call wrote nothing to the parent"
        );
        let mut delivered = line();
        BranchLine::fold(&mut delivered, &call(1, "call", "branch", steer));
        BranchLine::fold(&mut delivered, &result(2, "call", false));
        assert_eq!(delivered.steered.as_deref(), Some("turn"));
    }
}
