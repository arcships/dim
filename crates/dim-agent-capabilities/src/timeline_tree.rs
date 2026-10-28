use crate::{
    host::{HostError, HostFuture, ToolExecutionContext},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{
    AgentError, Branch, BranchOptions, Dispatcher, Names, Result, SessionIdentity, StoredBranch,
    Timeline, TimelineContext, TimelineFactory, TimelineInfo, TimelineKey, TimelineStore,
    TimelineTreeControl,
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
    /// rebuild from, and sends the `branch/create` sequences of the creates
    /// it recorded as failed: the restore checks its children against them.
    Resume {
        head: u64,
        failed_creates: oneshot::Sender<BTreeSet<u64>>,
    },
}

struct Node {
    info: TimelineInfo,
    /// The built branch, before and after publication.
    context: Option<Arc<TimelineContext>>,
    ready: watch::Sender<Option<Result<Branch>>>,
    started: bool,
}
struct TreeState {
    nodes: BTreeMap<String, Node>,
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
}
/// Shared with Timeline roots; never owns the tree or its child roots.
pub struct TreeCommands {
    tree: Weak<TimelineTree>,
}
#[service]
impl TreeCommands {
    #[service(skip)]
    pub fn get(&self, id: &str) -> Result<Branch> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .owned_context(id)
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
    /// The directory record of branch `id`.
    pub fn info(&self, id: String) -> std::result::Result<TimelineInfo, AgentError> {
        self.list()
            .into_iter()
            .find(|info| info.id == id)
            .ok_or(AgentError::UnknownBranch(id))
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
            let deadline = tokio::time::Instant::now() + owner.factory.startup_timeout();
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
                        _ = tokio::time::sleep_until(deadline) => {
                            return Err(AgentError::Build("branch opening timed out".into()));
                        }
                    }
                }
                if let Some((timeline, through)) = validate {
                    let context = tokio::select! {
                        result = crate::history::read_context(&timeline, through) => result?,
                        _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                        _ = tokio::time::sleep_until(deadline) => return Err(AgentError::Build("branch opening timed out".into())),
                    };
                    crate::history::validate_context_boundary(&context)
                        .map_err(AgentError::InvalidInput)?;
                }
                let built = owner
                    .factory
                    .build_until(id.clone(), ancestors, owner.cancel.child_token(), deadline)
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
                    Opening::Resume {
                        head,
                        failed_creates,
                    } => {
                        let history = tokio::select! {
                            result = context.timeline().local(..=head) => result?,
                            _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                            _ = tokio::time::sleep_until(deadline) => return Err(AgentError::Build("branch opening timed out".into())),
                        };
                        let failed = history
                            .iter()
                            .filter_map(|event| match &event.kind {
                                EventKind::BranchCreateFailed(
                                    BranchCreateFailedData::Failed { create_event, .. }
                                    | BranchCreateFailedData::Cancelled { create_event, .. },
                                ) => Some(*create_event),
                                _ => None,
                            })
                            .collect();
                        let _ = failed_creates.send(failed);
                        (EventKind::BranchResume(BranchResumeData {}), None)
                    }
                };
                let opened = context.timeline().append(opening.into())?;
                let mut dispatch = context.branch().dispatch_watch();
                loop {
                    let state = dispatch.borrow_and_update().clone();
                    if let Some(error) = state.failure {
                        return Err(error);
                    }
                    if state.sequence >= opened.sequence {
                        break;
                    }
                    tokio::select! {
                        changed = dispatch.changed() => if changed.is_err() {
                            return Err(AgentError::Closed("branch dispatch".into()));
                        },
                        _ = owner.cancel.cancelled() => {
                            return Err(AgentError::Cancelled("branch creation".into()));
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            return Err(AgentError::Build("branch opening timed out".into()));
                        }
                    }
                }
                tokio::select! {
                    result = context.finish_opening() => result?,
                    _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                    _ = tokio::time::sleep_until(deadline) => return Err(AgentError::Build("branch opening timed out".into())),
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
                let next = stored.info;
                if next.parent.as_ref().is_some_and(|parent| {
                    inactive.contains(parent)
                        || failed_creates.contains(&(
                            parent.clone(),
                            next.create_anchor.expect("validated creation anchor"),
                        ))
                }) {
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
                        let (failed_creates, reported) = oneshot::channel();
                        let head = stored.sequence;
                        (
                            Opening::Resume {
                                head,
                                failed_creates,
                            },
                            Some(reported),
                        )
                    };
                    self.launch(&mut state, next.id.clone(), ancestors, None, opening);
                    (ready, reported)
                };
                Self::await_branch(ready).await?;
                if let Some(reported) = reported {
                    let failed = reported.await.expect(
                        "a resumed branch reports its failed creates before it is published",
                    );
                    failed_creates
                        .extend(failed.into_iter().map(|create| (next.id.clone(), create)));
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
                    if admission.is_cancelled() {
                        return Err(AgentError::Cancelled("branch admission".into()));
                    }
                    ready = Some(self.reserve(
                        &mut self.state.lock().unwrap(),
                        TimelineInfo {
                            id: final_data.branch.clone(),
                            parent: Some(parent.clone()),
                            branch_point: Some(branch_point),
                            create_anchor: Some(event.sequence),
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
            dispatcher.subscribe(ctx, "TreeTimeline", &["branch/create"], move |event| {
                let result = commands
                    .tree
                    .upgrade()
                    .ok_or_else(|| AgentError::Closed("TimelineTree".into()))
                    .and_then(|tree| tree.on_create(&identity.branch_id, &timeline, &event));
                Box::pin(async move { result })
            })?;
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
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let factory = ctx.require_at(&self.factory)?;
            let store = ctx.require_at(&self.store)?;
            let identity = ctx.require_at(&self.identity)?;
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
}
impl BranchToolPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
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
                    definition: branch_definition(),
                    handler: Arc::new(BranchTool { tree }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
struct BranchTool {
    tree: Arc<TreeCommands>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchInput {
    prompt: String,
    at: Option<u64>,
}
impl ToolHandler for BranchTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let input: BranchInput = serde_json::from_value(serde_json::Value::Object(call.input))
                .map_err(|error| HostError::failed(format!("Invalid branch arguments: {error}")))?;
            if input.prompt.is_empty() || input.at.is_some_and(|at| at > 9_007_199_254_740_991) {
                return Err(HostError::failed(
                    "branch requires a nonempty prompt and a safe integer position",
                ));
            }
            crate::context_policy::check_cancel(&cancel)?;
            let parent = self
                .tree
                .get(&context.branch_id)
                .map_err(|e| HostError::failed(e.to_string()))?;
            let at = match input.at {
                Some(at) => at,
                None => {
                    let events = parent
                        .local(..)
                        .await
                        .map_err(|e| HostError::failed(e.to_string()))?;
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
                    context.branch_id,
                    at,
                    BranchOptions {
                        prompt: input.prompt,
                        source: BranchSource::Tool {
                            turn_id: context.turn_id,
                            request_id: context.request_id,
                            call_id: call.id,
                        },
                    },
                    cancel,
                )
                .await
                .map_err(|error| match error {
                    AgentError::Cancelled(reason) => HostError::Cancelled(reason),
                    error => HostError::failed(error.to_string()),
                })?;
            Ok(ToolResult::text(format!("Branch created: {}", branch.id())))
        })
    }
}
/// The child's first input. A branch the model created works on a task its
/// parent delegated; a host-created branch starts with the operator's words.
fn first_input(parent: &str, data: &BranchCreateData) -> InputMessageData {
    match data.source {
        BranchSource::Tool { .. } => InputMessageData {
            blocks: vec![crate::context_block::raw(
                crate::context_block::element(
                    "dim-branch-task",
                    &[("parent", parent)],
                    &format!(
                        "You are a branch forked from {} at this point of its history. That branch delegated this task to you:\n{}",
                        crate::context_block::text(parent),
                        crate::context_block::text(&data.prompt)
                    ),
                ),
                None,
            )],
            mode: Some(InputMode::Queue),
            ..InputMessageData::default()
        },
        BranchSource::Host { .. } => {
            InputMessageData::text(data.prompt.clone(), Some(InputMode::Queue))
        }
    }
}

fn branch_definition() -> ToolDefinition {
    ToolDefinition {
        risk: Some("session".into()),
        ..crate::tool_registry::definition(
            "branch",
            "Create an independent branch that continues from your timeline and works on prompt as a task you delegate. The result contains its branch ID.",
            serde_json::json!({"type":"object","required":["prompt"],"additionalProperties":false,"properties":{"prompt":{"type":"string","description":"The task the branch works on"},"at":{"type":"number","minimum":0}}}),
        )
    }
}
