pub mod read;

use crate::{
    host::{HostError, HostFuture, ToolExecutionContext},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{
    AgentError, Branch, BranchDirectoryEntry, BranchDirectoryState, BranchLifecycle, BranchOptions,
    Dispatcher, Names, QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage,
    QueryPick, QuerySelect, QuerySpec, QueryValue, Result, SessionIdentity, StoredBranch,
    StoredEvent, Timeline, TimelineContext, TimelineFactory, TimelineIdentity, TimelineInfo,
    TimelineKey, TimelineQuery, TimelineQueryScope, TimelineStore, TimelineTreeControl,
};
use dim_agent_types::*;
use futures_util::{FutureExt, future::join_all};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::service;
use rutis::{Ctx, Error};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
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

/// How a loaded branch begins on its Timeline.
enum Opening {
    /// A new branch: the root of a new Session, or a child with its first
    /// input.
    Start {
        start: BranchStartData,
        first: Option<InputMessageData>,
    },
    Resume,
}

struct Node {
    info: TimelineInfo,
    opening_idle_notice: IdleNotice,
    /// The built branch, before and after publication.
    context: Option<Arc<TimelineContext>>,
    ready: watch::Sender<Option<Result<Branch>>>,
    /// The sequence of the line's latest `turn/end`, 0 before the first.
    turn_end: watch::Sender<u64>,
    started: bool,
}
struct EndedLine {
    head: u64,
    info: TimelineInfo,
    end_event: Option<u64>,
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
        self.branch_with_idle_notice(parent, at, options, admission, IdleNotice::Steer)
            .await
    }
    async fn branch_with_idle_notice(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: CancellationToken,
        idle_notice: IdleNotice,
    ) -> Result<Branch> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .create_child(parent, at, options, admission, idle_notice)
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
    /// Commits `inputs` to branch `id` as one atomic batch through its
    /// validating `Branch::inputs`: the given order, contiguous sequences and
    /// one publication. Rust only: no other language has a consumer.
    #[service(skip)]
    pub async fn inputs(
        &self,
        id: String,
        inputs: Vec<InputMessageData>,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.get(&id)?.inputs(inputs).await
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
    /// One page of `id`'s committed batches up to `through`; an ended line
    /// reads at its fixed head and `through` must end an atomic batch.
    pub async fn page(
        &self,
        id: String,
        after: u64,
        limit: usize,
        through: u64,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .page(&id, after, limit, through)
            .await
    }
    /// Ends a raw-event page on an atomic batch boundary without reading its payloads.
    #[service(skip)]
    pub async fn page_end(
        &self,
        id: &str,
        after: u64,
        limit: std::num::NonZeroU64,
        through: u64,
    ) -> Result<u64> {
        let tree = self
            .tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?;
        let head = tree
            .head(id)
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
        if after > through || through > head {
            return Err(AgentError::InvalidInput("future Timeline cursor".into()));
        }
        let target = after.saturating_add(limit.get()).min(through);
        if let Some(context) = tree.owned_context(id) {
            let timeline = context.timeline();
            timeline.batch_end(after).await?;
            timeline.batch_end(through).await?;
            timeline.batch_end_at(target).await
        } else {
            let key = TimelineKey {
                session_id: tree.session_id.clone(),
                branch_id: id.into(),
            };
            tree.store.batch_end(&key, after).await?;
            tree.store.batch_end(&key, through).await?;
            tree.store.batch_end_at(&key, target).await
        }
    }
    /// Computes properties of one line at a fixed cutoff. Chain scope is invalid.
    /// An ended line reads its persisted facts and has no publishing tail.
    pub async fn query(
        &self,
        id: String,
        query: dim_agent_runtime::TimelineQuery,
    ) -> std::result::Result<dim_agent_runtime::TimelineQueryReply, AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .query(&id, query)
            .await
    }
    /// The head a read of `id` ends at: fixed once ended, published while
    /// open.
    #[service(skip)]
    fn head(&self, id: &str) -> Option<u64> {
        self.tree.upgrade()?.head(id)
    }
    /// Checks that `sequence` ends an atomic batch of line `id`, forwarding to
    /// its Timeline while open and reading the store once it ends. The check
    /// stays out of the service surface because no other language reads batch
    /// structure.
    #[service(skip)]
    pub async fn batch_end(
        &self,
        id: String,
        sequence: u64,
    ) -> std::result::Result<(), AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .batch_end(&id, sequence)
            .await
    }
    /// A signal that changes when line `id` ends a turn; it carries that
    /// `turn/end`'s sequence.
    #[service(skip)]
    fn turn_ends(&self, id: &str) -> Option<watch::Receiver<u64>> {
        self.tree.upgrade()?.turn_ends(id)
    }
    /// Whether line `id` did anything after `from` that a line it opened must
    /// see: an operator input, or a tool call.
    #[service(skip)]
    async fn activity_after(
        &self,
        id: &str,
        from: u64,
        through: u64,
    ) -> std::result::Result<bool, AgentError> {
        self.tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?
            .activity_after(id, from, through)
            .await
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
/// One page of an open line's own facts after `after`, at most `limit`, never
/// past `through`, ending on an atomic batch: a page that reaches its limit
/// inside one runs to that batch's end, so an open and an ended line's pages
/// keep the same shape.
async fn open_page(
    timeline: &Timeline,
    after: u64,
    limit: usize,
    through: u64,
) -> Result<Vec<Arc<AgentEvent>>> {
    if after >= through {
        return Ok(vec![]);
    }
    let mut events = Vec::new();
    let mut from = after + 1;
    let mut wanted = limit;
    loop {
        let reply = timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(TimelinePosition {
                    branch_id: timeline.branch_id(),
                    sequence: from,
                }),
                through: Some(through),
                include_publishing: false,
                query: QuerySpec {
                    filter: None,
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::new(wanted as u64).expect("a page keeps one fact"),
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Events { events: rows } = reply.result else {
            unreachable!("event selection returns events")
        };
        if rows.is_empty() {
            break;
        }
        events.extend(rows.into_iter().map(|row| row.event));
        let last = events.last().unwrap().sequence;
        if last >= through {
            break;
        }
        match timeline.batch_end(last).await {
            Ok(()) => break,
            Err(AgentError::InvalidInput(_)) => {}
            Err(error) => return Err(error),
        }
        from = last + 1;
        wanted = 1;
    }
    Ok(events)
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
    async fn child_terminal(
        &self,
        parent: &str,
        child: &str,
        head: u64,
    ) -> Result<Option<EventKind>> {
        let reply = self
            .store
            .query(
                &self.stored_scope(parent, 1, head),
                &QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["branch/create-failed".into(), "branch/ended".into()],
                            },
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.branch".into(),
                                },
                                values: vec![child.into()],
                            },
                            QueryFilter::TextCandidate {
                                field: "data".into(),
                                partition_by: vec![],
                                terms: vec![child.into()],
                            },
                        ],
                    }),
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: Some(QueryPick::Last),
                    order_by: vec![],
                },
                Some(&QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            )
            .await?;
        let QueryOutput::Events { events } = reply.result else {
            unreachable!("child terminal fact")
        };
        Ok(events
            .into_iter()
            .next()
            .map(|event| event.event.kind.clone()))
    }
    fn stored_scope(
        &self,
        branch: &str,
        from: u64,
        through: u64,
    ) -> dim_agent_runtime::TimelineReadScope {
        dim_agent_runtime::TimelineReadScope {
            key: TimelineKey {
                session_id: self.session_id.clone(),
                branch_id: branch.into(),
            },
            through,
            segments: vec![dim_agent_runtime::TimelineReadSegment {
                branch_id: branch.into(),
                from,
                through,
                stored_through: through,
                tail: vec![],
            }],
        }
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
        for id in Self::stored_descendants(&line.id, stored) {
            let parent = stored[&id]
                .info
                .parent
                .clone()
                .expect("a stored descendant has a parent");
            if matches!(
                self.child_terminal(&parent, &id, stored[&parent].sequence)
                    .await?,
                Some(EventKind::BranchEnded(_))
            ) {
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
            parent_call_id: None,
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
    /// Whether a Queue input follows this stored child's branch start.
    /// Append inputs written by Prompt do not settle creation.
    async fn created(&self, branch: &str, head: u64) -> Result<bool> {
        let scope = self.stored_scope(branch, 1, head);
        let reply = self
            .store
            .query(
                &scope,
                &QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["input/message".into()],
                            },
                            QueryFilter::Or {
                                conditions: vec![
                                    QueryFilter::In {
                                        value: QueryOperand::Field {
                                            path: "data.mode".into(),
                                        },
                                        values: vec!["queue".into()],
                                    },
                                    QueryFilter::And {
                                        conditions: vec![
                                            QueryFilter::Not {
                                                condition: Box::new(QueryFilter::Present {
                                                    value: QueryOperand::Field {
                                                        path: "data.mode".into(),
                                                    },
                                                }),
                                            },
                                            QueryFilter::Present {
                                                value: QueryOperand::Field {
                                                    path: "data.user.content".into(),
                                                },
                                            },
                                        ],
                                    },
                                ],
                            },
                            QueryFilter::Exists {
                                condition: Box::new(QueryFilter::And {
                                    conditions: vec![
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "kind".into(),
                                            },
                                            values: vec!["branch/start".into()],
                                        },
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "position.sequence".into(),
                                            },
                                            comparison: QueryComparison::Lt,
                                            right: QueryOperand::Outer {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                    ],
                                }),
                            },
                        ],
                    }),
                    select: QuerySelect::Exists,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                None,
            )
            .await?;
        let QueryOutput::Exists { exists } = reply.result else {
            unreachable!("existence selection returns a boolean")
        };
        Ok(exists)
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
        idle_notice: IdleNotice,
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
        let (turn_end, _) = watch::channel(0);
        state.nodes.insert(
            info.id.clone(),
            Node {
                info,
                opening_idle_notice: idle_notice,
                context: None,
                ready,
                turn_end,
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
                if let Some((timeline, through)) = &validate {
                    let boundary = tokio::select! {
                        result = tokio::time::timeout(owner.factory.startup_timeout(), crate::history::validate_context_boundary_at(timeline, *through)) =>
                            result.map_err(|_| AgentError::Build("branch opening timed out".into()))??,
                        _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                    };
                    boundary.map_err(AgentError::InvalidInput)?;
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
                let (opening, mut first) = match opening {
                    Opening::Start { start, first } => (EventKind::BranchStart(start), first),
                    Opening::Resume => (EventKind::BranchResume(BranchResumeData {}), None),
                };
                if let (Some(parent), Some((timeline, cut)), Some(input)) =
                    (&info.parent, &validate, &mut first)
                {
                    // A nested line inherits its parent's parent block, which
                    // names a line it cannot read; its own opening input
                    // supersedes that state with one naming the line that
                    // opened it (§7.1.1). The read runs here, as a resumed
                    // line's facts do, so a failed read fails this create
                    // through the existing path and the parent keeps
                    // dispatching.
                    let key = parent_key();
                    let carried = tokio::select! {
                        result = tokio::time::timeout(owner.factory.startup_timeout(), owner.keyed_live(parent, timeline, *cut, &key)) =>
                            result.map_err(|_| AgentError::Build("branch opening timed out".into()))??,
                        _ = owner.cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into())),
                    };
                    if carried {
                        input.blocks.push(parent_open_block(parent, *cut));
                    }
                }
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
    /// One page of `id`'s committed batches, never past `through`: an ended
    /// line reads at its fixed head. `through` must end an atomic batch, as it
    /// does for a live line's read, so a page cannot be cut inside one.
    async fn page(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let head = self
            .head(id)
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
        if through > head {
            return Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            )));
        }
        if through <= after {
            return Ok(Vec::new());
        }
        if let Some(context) = self.owned_context(id) {
            context.timeline().batch_end(through).await?;
            return open_page(context.timeline(), after, limit, through).await;
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
    async fn query(
        &self,
        id: &str,
        query: TimelineQuery,
    ) -> Result<dim_agent_runtime::TimelineQueryReply> {
        if query.scope != TimelineQueryScope::Branch {
            return Err(AgentError::InvalidInput(
                "a tree query reads one line's own facts".into(),
            ));
        }
        if let Some(context) = self.owned_context(id) {
            return context.timeline().query(query).await;
        }
        let head = self
            .ended_head(id)
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))?;
        query.query.validate(query.page.as_ref())?;
        if query.through.is_none() && query.page.as_ref().is_some_and(|page| page.after.is_some()) {
            return Err(AgentError::InvalidInput(
                "continued queries require the original through".into(),
            ));
        }
        let through = query.through.unwrap_or(head);
        if through > head {
            return Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            )));
        }
        let from = match query.from {
            Some(position)
                if position.branch_id == id
                    && position.sequence > 0
                    && position.sequence <= through =>
            {
                position.sequence
            }
            Some(_) => {
                return Err(AgentError::InvalidInput(
                    "query start is outside its branch scope or sequence range".into(),
                ));
            }
            None => 1,
        };
        let scope = self.stored_scope(id, from, through);
        let result = self
            .store
            .query(&scope, &query.query, query.page.as_ref())
            .await?;
        Ok(dim_agent_runtime::TimelineQueryReply {
            through,
            result: result.result,
            next: result.next,
        })
    }
    /// The batch-end check of one line, forwarding to its Timeline while open
    /// and reading the store at the tree's key after it ends.
    async fn batch_end(&self, id: &str, sequence: u64) -> Result<()> {
        if let Some(context) = self.owned_context(id) {
            return context.timeline().batch_end(sequence).await;
        }
        if self.ended_head(id).is_none() {
            return Err(AgentError::UnknownBranch(id.into()));
        }
        let key = TimelineKey {
            session_id: self.session_id.clone(),
            branch_id: id.into(),
        };
        self.store.batch_end(&key, sequence).await
    }
    /// Whether `id` did anything after `from` up to `through` that a line it
    /// opened must see ([timeline branches §7.1.1](../../../docs/design/timeline-branches.md#711-the-parent-block-branchparent)):
    /// an operator input, or a tool call.
    async fn activity_after(&self, id: &str, from: u64, through: u64) -> Result<bool> {
        if through <= from {
            return Ok(false);
        }
        let scope = self.stored_scope(id, from + 1, through);
        let reply = self
            .store
            .query(
                &scope,
                &QuerySpec {
                    filter: Some(QueryFilter::Or {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["step/tool-use".into()],
                            },
                            QueryFilter::And {
                                conditions: vec![
                                    QueryFilter::In {
                                        value: QueryOperand::Field {
                                            path: "kind".into(),
                                        },
                                        values: vec!["input/message".into()],
                                    },
                                    QueryFilter::Present {
                                        value: QueryOperand::Field {
                                            path: "data.user.content".into(),
                                        },
                                    },
                                ],
                            },
                        ],
                    }),
                    select: QuerySelect::Exists,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                None,
            )
            .await?;
        let QueryOutput::Exists { exists } = reply.result else {
            unreachable!("existence selection returns a boolean")
        };
        Ok(exists)
    }
    /// Whether the latest block under `key` in the context `parent` has built
    /// at `cut` is live: the keyed state, resolved over the chain the child
    /// inherits, the state a block written under the same key supersedes. The
    /// blocks are read as blocks, never as the rendered history.
    async fn keyed_live(
        &self,
        parent: &str,
        timeline: &Timeline,
        cut: u64,
        key: &str,
    ) -> Result<bool> {
        let mut chain = timeline.ancestors();
        chain.push((parent.to_owned(), cut));
        for (line, through) in chain.into_iter().rev() {
            if self.info(&line).is_none_or(|info| info.parent.is_none()) {
                continue;
            }
            let scope = self.stored_scope(&line, 1, through);
            let reply = self
                .store
                .query(
                    &scope,
                    &QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["input/message".into()],
                                },
                                QueryFilter::Any {
                                    field: "data.blocks".into(),
                                    condition: Box::new(QueryFilter::In {
                                        value: QueryOperand::Element { path: "key".into() },
                                        values: vec![key.into()],
                                    }),
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec!["data.blocks[].key".into(), "data.blocks[].retire".into()],
                        },
                        group_by: vec![],
                        pick: Some(QueryPick::Last),
                        order_by: vec![],
                    },
                    Some(&QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                )
                .await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("field selection returns rows")
            };
            if let Some(row) = rows.first() {
                let [
                    QueryValue::Elements { values: keys },
                    QueryValue::Elements { values: retires },
                ] = row.values.as_slice()
                else {
                    unreachable!("input block projections retain array positions")
                };
                for (found, retire) in keys.iter().zip(retires).rev() {
                    if matches!(found, QueryValue::Value { value: serde_json::Value::String(found) } if found == key)
                    {
                        return Ok(!matches!(
                            retire,
                            QueryValue::Value {
                                value: serde_json::Value::Bool(true)
                            }
                        ));
                    }
                }
                unreachable!("selected input has a matching keyed block");
            }
        }
        Ok(false)
    }
    fn head(&self, id: &str) -> Option<u64> {
        if let Some(ended) = self.state.lock().unwrap().ended.get(id) {
            return Some(ended.head);
        }
        self.owned_context(id)
            .map(|context| context.timeline().sequence())
    }
    fn turn_ends(&self, id: &str) -> Option<watch::Receiver<u64>> {
        self.state
            .lock()
            .unwrap()
            .nodes
            .get(id)
            .map(|node| node.turn_end.subscribe())
    }
    async fn own_end(&self, branch: &str, head: u64) -> Result<Option<(u64, BranchEndData)>> {
        let scope = self.stored_scope(branch, 1, head);
        let reply = self
            .store
            .query(
                &scope,
                &QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["branch/end".into()],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["position.sequence".into(), "data".into()],
                    },
                    group_by: vec![],
                    pick: Some(QueryPick::Last),
                    order_by: vec![],
                },
                Some(&QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            )
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("field selection returns rows")
        };
        Ok(rows.into_iter().next().map(|row| {
            let [
                QueryValue::Value { value: sequence },
                QueryValue::Value { value: data },
            ] = row.values.as_slice()
            else {
                unreachable!("branch end has a sequence and data")
            };
            (
                sequence.as_u64().expect("event sequence is unsigned"),
                serde_json::from_value(data.clone())
                    .expect("branch end follows its event contract"),
            )
        }))
    }
    async fn creation_source(
        &self,
        parent: &str,
        child: &str,
        anchor: u64,
    ) -> Result<Option<BranchSource>> {
        let scope = self.stored_scope(parent, anchor, anchor);
        let reply = self
            .store
            .query(
                &scope,
                &QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["branch/create".into()],
                            },
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.branch".into(),
                                },
                                values: vec![child.into()],
                            },
                        ],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["data.source".into()],
                    },
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                Some(&QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            )
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("field selection returns rows")
        };
        Ok(rows.into_iter().next().map(|row| {
            let [QueryValue::Value { value: source }] = row.values.as_slice() else {
                unreachable!("branch create has a source")
            };
            serde_json::from_value(source.clone())
                .expect("branch source follows its event contract")
        }))
    }
    /// The stored head of an ended line, `None` for an open, ending or unknown
    /// one.
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
        context.timeline().append_registered(
            EventKind::BranchEnd(data.clone()).into(),
            |event| {
                if context.ended() {
                    return Err(AgentError::BranchEnded(id.into()));
                }
                context.mark_ended(event.sequence);
                Ok(())
            },
        )?;
        loop {
            let notified = self.end_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                // After the commit above, `on_end` parks a tool-sourced end of
                // a running line in `pending`; the tool call belongs to the
                // turn the close waits for, so `end()` returns there.
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
    /// Closes a line whose end command is part of the turn that just ended,
    /// and lets the line's children see that it moved.
    fn on_turn_end(self: &Arc<Self>, id: &str, sequence: u64) {
        let pending = {
            let mut state = self.state.lock().unwrap();
            if let Some(node) = state.nodes.get(id) {
                node.turn_end.send_replace(sequence);
            }
            state.pending.remove(id)
        };
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
        let sequence = context.timeline().append_registered(
            EventKind::BranchEnd(data.clone()).into(),
            |event| {
                if context.ended() {
                    return Err(AgentError::BranchEnded(id.into()));
                }
                context.mark_ended(event.sequence);
                Ok(())
            },
        );
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
                    end_event: Some(end_event),
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
    fn create_child(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: CancellationToken,
        idle_notice: IdleNotice,
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
            crate::history::validate_context_boundary_at(timeline, at)
                .await?
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
                        idle_notice,
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
        let Some(node) = state
            .nodes
            .get_mut(&data.branch)
            .filter(|node| !node.started)
        else {
            return Ok(());
        };
        let idle_notice = std::mem::take(&mut node.opening_idle_notice);
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
                first: Some(first_input(parent, data, idle_notice)),
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
                    IdleNotice::Steer,
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
                let terminal = if let Some(parent) = &next.parent {
                    let head = self.head(parent).unwrap_or(0);
                    next.source = self
                        .creation_source(
                            parent,
                            &next.id,
                            next.create_anchor.expect("child creation position"),
                        )
                        .await?;
                    self.child_terminal(parent, &next.id, head).await?
                } else {
                    None
                };
                if let Some(EventKind::BranchEnded(ended)) = &terminal {
                    self.state.lock().unwrap().ended.insert(
                        next.id.clone(),
                        EndedLine {
                            head: stored.sequence,
                            info: next.clone(),
                            end_event: Some(ended.end_event),
                        },
                    );
                    remaining.remove(&next.id);
                    inactive.insert(next.id);
                    continue;
                }
                if next.parent.as_ref().is_some_and(|parent| {
                    inactive.contains(parent)
                        || matches!(terminal, Some(EventKind::BranchCreateFailed(_)))
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
                        // A cascade-ended line's `branch/end` sits on the
                        // line itself; one page reads it (limit 1). The
                        // result, if any, is what the directory reports.
                        let end_event = self
                            .own_end(&next.id, stored.sequence)
                            .await?
                            .map(|(sequence, _)| sequence);
                        self.state.lock().unwrap().ended.insert(
                            next.id.clone(),
                            EndedLine {
                                head: stored.sequence,
                                info: next.clone(),
                                end_event,
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
                            end_event: Some(end_event),
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
                        let mut ancestors = timeline.ancestors();
                        ancestors.push((parent.clone(), cut));
                        ancestors
                    }
                    None => vec![],
                };
                let ready = {
                    let mut state = self.state.lock().unwrap();
                    let ready = self.reserve(&mut state, next.clone(), IdleNotice::Steer)?;
                    let opening = if next.parent.is_none() && stored.sequence == 0 {
                        Opening::Start {
                            start: BranchStartData {
                                parent: None,
                                create_event: None,
                            },
                            first: None,
                        }
                    } else {
                        Opening::Resume
                    };
                    self.launch(&mut state, next.id.clone(), ancestors, None, opening);
                    ready
                };
                Self::await_branch(ready).await?;
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
        self.create_child(parent, at, options, admission, IdleNotice::Steer)
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
    fn directory(&self) -> Vec<BranchDirectoryEntry> {
        // Collect the records under the lock, then resolve heads through
        // `head()`, which takes the lock itself.
        let mut entries: Vec<BranchDirectoryEntry> = {
            let state = self.state.lock().unwrap();
            let mut entries: Vec<BranchDirectoryEntry> = state
                .nodes
                .values()
                .filter(|node| matches!(&*node.ready.borrow(), Some(Ok(_))))
                .map(|node| {
                    let end_event = node
                        .context
                        .as_ref()
                        .and_then(|context| context.end_event());
                    BranchDirectoryEntry {
                        info: node.info.clone(),
                        head: 0,
                        state: match end_event {
                            Some(_) => BranchDirectoryState::Ending,
                            None => BranchDirectoryState::Open,
                        },
                        end_event,
                    }
                })
                .collect();
            for ended in state.ended.values() {
                entries.push(BranchDirectoryEntry {
                    info: ended.info.clone(),
                    head: ended.head,
                    state: BranchDirectoryState::Ended,
                    end_event: ended.end_event,
                });
            }
            entries
        };
        for entry in &mut entries {
            if entry.state != BranchDirectoryState::Ended {
                entry.head = self.head(&entry.info.id).unwrap_or(0);
            }
        }
        entries.sort_by(|a, b| {
            let order = |id: &str| id != "main";
            (order(&a.info.id), &a.info.id).cmp(&(order(&b.info.id), &b.info.id))
        });
        entries
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
                            .map(|tree| tree.on_turn_end(branch, event.sequence)),
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
            guide.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-branch-parent".into()],
                    order: 45,
                    text: PARENT_ENTRY.into(),
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
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum IdleNotice {
    #[default]
    Steer,
    None,
}
const QUIET_IDLE_ATTRIBUTE: &str = "idle-notice";
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
    Parent,
}
/// BranchLine's fold of its own line's facts.
#[derive(Default)]
struct LineView {
    live_turn: Option<String>,
    role: Option<LineRole>,
    parent: Option<String>,
    idle_notice: IdleNotice,
    /// The lines this line opened, open or not, by the order it opened them.
    children: BTreeMap<String, Child>,
    roster: Option<(String, bool)>,
    /// The turn that steered the parent, when one did.
    steered: Option<String>,
    steering: Option<(String, String)>,
    /// The `nextCursor` of the latest successful read of each line, by line.
    cursors: BTreeMap<String, u64>,
    /// The line a pending `branch` read names, by its call id.
    reads: BTreeMap<String, String>,
    /// The event id of the latest `branch:parent` block on this line.
    parent_input: Option<String>,
    /// The parent head the latest `branch:parent` block reported.
    parent_told: u64,
    /// Inputs committed and not consumed, with their modes.
    held: BTreeMap<String, InputMode>,
    /// The sequence of each input's `input/applied`.
    applied: BTreeMap<String, u64>,
    /// The line committed its own `branch/end`: it is ending.
    ending: bool,
}
impl LineView {
    fn input_block(&mut self, id: &str, text: &str, key: &str, retire: bool) {
        if key == line_key() {
            if self.role == Some(LineRole::Tool)
                && self.roster.is_none()
                && !retire
                && text.lines().next().is_some_and(|line| {
                    line.starts_with("<dim-branch-line ")
                        && line.ends_with(r#" idle-notice="none">"#)
                })
            {
                self.idle_notice = IdleNotice::None;
            }
            self.roster = Some((text.to_owned(), retire));
        } else if key == parent_key() {
            self.parent_input = Some(id.to_owned());
            if let Some(head) = parent_head(text) {
                self.parent_told = head;
            }
        }
    }
    fn interrupt(&mut self, sequence: u64) -> Option<Decision> {
        let released = self
            .parent_input
            .clone()
            .filter(|id| self.held.contains_key(id));
        let applied = &self.applied;
        self.held
            .retain(|id, _| applied.get(id).is_none_or(|at| *at > sequence));
        released
            .filter(|id| !self.held.contains_key(id))
            .map(|_| Decision::Parent)
    }
    fn tool_result(&mut self, call_id: &str, error: bool, next: Option<u64>) {
        if self
            .steering
            .as_ref()
            .is_some_and(|(_, call)| call == call_id)
        {
            let (turn, _) = self.steering.take().unwrap();
            if !error {
                self.steered = Some(turn);
            }
        }
        if let Some(target) = self.reads.remove(call_id)
            && !error
            && let Some(next) = next
        {
            self.cursors.insert(target, next);
        }
    }

    /// Whether input `id` is the parent block and is still held.
    fn holds_parent(&self, id: &str) -> bool {
        self.parent_input.as_deref() == Some(id) && self.held.contains_key(id)
    }
    /// Drops input `id` from what is held, and queues the parent decision when
    /// the dropped input was the parent block (design §7.1.1).
    fn release(&mut self, id: &str) -> Option<Decision> {
        let released = self.holds_parent(id);
        self.held.remove(id);
        self.applied.remove(id);
        released.then_some(Decision::Parent)
    }
}
/// `<dim-branch-line>`, written by TimelineTree at the start of every child.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;

/// `<dim-branch-lines>`, the roster BranchLine writes on main.
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="main">: on main, the runtime lists the lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;

/// The roster's closing sentence, coordination text for `main`
/// ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)),
/// replaceable at [`BranchToolConfig::roster_closing`].
const ROSTER_CLOSING: &str = "They message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you.";

/// `<dim-branch-message>`, `<dim-branch-idle>` and `<dim-branch-ended>`.
const BRANCH_MESSAGE_ENTRY: &str = r#"<dim-branch-message from="C" to="main">: a message from another line of this session, in that line's own words and not the operator's. dim-branch-idle and dim-branch-ended are the runtime's reports about a line you opened: information, not requests."#;

/// `<dim-branch-parent>`, written by BranchLine on a line whose parent moved.
const PARENT_ENTRY: &str = r#"<dim-branch-parent branch="P" head="H" after="A">: on a line, the runtime reports that the line it was cut from has moved past where this line last read it, and where the next read starts. A later block of this name replaces the earlier one; it is state about that line, not a request."#;

pub struct BranchLinePlugin {
    line: Key<BranchLine>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
    lifecycle: Key<BranchLifecycle>,
    session: Key<dim_agent_runtime::SessionLifecycle>,
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
            timeline: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
            lifecycle: names.key()?,
            session: names.key()?,
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
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.identity.erase(),
            self.lifecycle.erase(),
            self.session.erase(),
            self.tree.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let session = ctx.require_at(&self.session)?;
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
            let read_timeline = timeline.clone();
            dispatcher
                .consume(
                    ctx,
                    "BranchLine".into(),
                    [
                        "branch/create",
                        "branch/create-failed",
                        "branch/ended",
                        "branch/end",
                        "input/message",
                        "input/applied",
                        "input/consumed",
                        "input/deleted",
                        "stop/interrupt",
                        "turn/start",
                        "turn/end",
                        "step/tool-use",
                        "step/tool-result",
                    ]
                    .map(String::from)
                    .to_vec(),
                    Arc::new(move || {
                        let timeline = read_timeline.clone();
                        Box::pin(async move { Ok(timeline.committed()) })
                    }),
                    Arc::new(move |event| {
                        let consumer = consumer.clone();
                        Box::pin(async move {
                            consumer.consume(&event);
                            Ok(())
                        })
                    }),
                )
                .await?;
            line.want(Decision::Roster);
            line.want(Decision::Parent);
            let writer = line.clone();
            let phase = lifecycle.clone();
            let tasks = TaskTracker::new();
            tasks.spawn(async move {
                tokio::select! {
                    _ = writer.stop.cancelled() => return,
                    active = phase.activated() => if !active { return },
                }
                if !session.activated().await {
                    return;
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
    async fn block(&self, key: &str) -> Result<Option<(String, String, bool)>> {
        let reply = self
            .tree
            .query(
                self.own.clone(),
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: None,
                    through: self.tree.head(&self.own),
                    include_publishing: true,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["input/message".into()],
                                },
                                QueryFilter::TextCandidate {
                                    field: "data".into(),
                                    partition_by: vec![],
                                    terms: vec!["branch".into()],
                                },
                                QueryFilter::Any {
                                    field: "data.blocks".into(),
                                    condition: Box::new(QueryFilter::In {
                                        value: QueryOperand::Element { path: "key".into() },
                                        values: vec![key.into()],
                                    }),
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: [
                                "id",
                                "data.blocks[].key",
                                "data.blocks[].text",
                                "data.blocks[].retire",
                            ]
                            .map(String::from)
                            .to_vec(),
                        },
                        group_by: vec![],
                        pick: Some(QueryPick::Last),
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                },
            )
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("branch block fields")
        };
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let [
            QueryValue::Value {
                value: serde_json::Value::String(id),
            },
            QueryValue::Elements { values: keys },
            QueryValue::Elements { values: texts },
            QueryValue::Elements { values: retires },
        ] = row.values.as_slice()
        else {
            unreachable!("branch block projections")
        };
        for ((found, text), retire) in keys.iter().zip(texts).zip(retires).rev() {
            if matches!(found, QueryValue::Value { value: serde_json::Value::String(found) } if found == key)
            {
                let QueryValue::Value {
                    value: serde_json::Value::String(text),
                } = text
                else {
                    unreachable!("raw block text")
                };
                return Ok(Some((
                    id.clone(),
                    text.clone(),
                    matches!(
                        retire,
                        QueryValue::Value {
                            value: serde_json::Value::Bool(true)
                        }
                    ),
                )));
            }
        }
        unreachable!("selected branch block")
    }
    async fn parent_held(&self, id: &str) -> Result<bool> {
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: self.tree.head(&self.own),
            include_publishing: true,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Field { path: "id".into() },
                            values: vec![id.into()],
                        },
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: vec!["input/message".into()],
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Exists {
                                condition: Box::new(QueryFilter::And {
                                    conditions: vec![
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "kind".into(),
                                            },
                                            values: vec![
                                                "input/consumed".into(),
                                                "input/deleted".into(),
                                            ],
                                        },
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "data.input_id".into(),
                                            },
                                            comparison: QueryComparison::Eq,
                                            right: QueryOperand::Outer { path: "id".into() },
                                        },
                                    ],
                                }),
                            }),
                        },
                    ],
                }),
                select: QuerySelect::Exists,
                group_by: vec![],
                pick: None,
                order_by: vec![],
            },
            page: None,
        };
        let QueryOutput::Exists { exists } = self
            .tree
            .query(self.own.clone(), query.clone())
            .await?
            .result
        else {
            unreachable!("parent input ownership")
        };
        if !exists {
            return Ok(false);
        }
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec!["input/applied".into()],
                },
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.input_id".into(),
                    },
                    values: vec![id.into()],
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec!["position.sequence".into()],
        };
        query.query.pick = Some(QueryPick::Last);
        query.page = Some(QueryPage {
            limit: NonZeroU64::MIN,
            after: None,
        });
        let QueryOutput::Rows { rows } = self
            .tree
            .query(self.own.clone(), query.clone())
            .await?
            .result
        else {
            unreachable!("parent handover")
        };
        let Some(row) = rows.first() else {
            return Ok(true);
        };
        let [QueryValue::Value { value: applied }] = row.values.as_slice() else {
            unreachable!("handover position")
        };
        let applied = applied.as_u64().expect("handover sequence");
        query.from = Some(TimelinePosition {
            branch_id: self.own.clone(),
            sequence: applied,
        });
        query.query.filter = Some(QueryFilter::In {
            value: QueryOperand::Field {
                path: "kind".into(),
            },
            values: vec!["stop/interrupt".into()],
        });
        query.query.select = QuerySelect::Exists;
        query.query.pick = None;
        query.page = None;
        let QueryOutput::Exists { exists } = self.tree.query(self.own.clone(), query).await?.result
        else {
            unreachable!("handover interruption")
        };
        Ok(!exists)
    }
    async fn roster_state(&self) -> Result<()> {
        let children = self
            .tree
            .list()
            .into_iter()
            .filter(|info| {
                info.parent.as_deref() == Some(self.own.as_str())
                    && matches!(info.source, Some(BranchSource::Tool { .. }))
            })
            .collect::<Vec<_>>();
        if children.is_empty() && self.view.lock().unwrap().children.is_empty() {
            let reply = self
                .tree
                .query(
                    self.own.clone(),
                    TimelineQuery {
                        scope: TimelineQueryScope::Branch,
                        from: None,
                        through: self.tree.head(&self.own),
                        include_publishing: true,
                        query: QuerySpec {
                            filter: Some(QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["branch/create".into()],
                            }),
                            select: QuerySelect::Exists,
                            group_by: vec![],
                            pick: None,
                            order_by: vec![],
                        },
                        page: None,
                    },
                )
                .await?;
            if matches!(reply.result, QueryOutput::Exists { exists: false }) {
                return Ok(());
            }
        }
        for child in children {
            if self.view.lock().unwrap().children.contains_key(&child.id) {
                continue;
            }
            let at = child.create_anchor.expect("child creation position");
            let reply = self
                .tree
                .query(
                    self.own.clone(),
                    TimelineQuery {
                        scope: TimelineQueryScope::Branch,
                        from: Some(TimelinePosition {
                            branch_id: self.own.clone(),
                            sequence: at,
                        }),
                        through: Some(at),
                        include_publishing: true,
                        query: QuerySpec {
                            filter: None,
                            select: QuerySelect::Fields {
                                fields: vec!["data.prompt".into()],
                            },
                            group_by: vec![],
                            pick: None,
                            order_by: vec![],
                        },
                        page: Some(QueryPage {
                            limit: NonZeroU64::MIN,
                            after: None,
                        }),
                    },
                )
                .await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("child purpose")
            };
            let [
                QueryValue::Value {
                    value: serde_json::Value::String(prompt),
                },
            ] = rows[0].values.as_slice()
            else {
                unreachable!("creation prompt")
            };
            self.view
                .lock()
                .unwrap()
                .children
                .entry(child.id)
                .or_insert(Child {
                    source: child.source.expect("tool child source"),
                    purpose: first_line(prompt),
                    opened: at,
                    failed: false,
                    ended: false,
                });
        }
        if self.view.lock().unwrap().roster.is_none() {
            let observed = self.block(&line_key()).await?;
            let mut view = self.view.lock().unwrap();
            if view.roster.is_none() {
                view.roster = observed.map(|(_, text, retire)| (text, retire));
            }
        }
        Ok(())
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
                        purpose: first_line(&data.prompt),
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
                    {
                        view.input_block(&event.id, text, key, *retire);
                    }
                }
                None
            }
            EventKind::InputApplied(data) => {
                view.applied.insert(data.input_id.clone(), event.sequence);
                None
            }
            EventKind::StopInterrupt(_) => view.interrupt(event.sequence),
            EventKind::InputConsumed(data) => view.release(&data.input_id),
            EventKind::InputDeleted(data) => view.release(&data.input_id),
            EventKind::ToolUse(data) => {
                if let Some(parent) = view.parent.clone()
                    && steered_parent(&data.call, &parent)
                {
                    view.steering = Some((data.turn_id.clone(), data.call.id.clone()));
                }
                if let Some(target) = read_target(&data.call) {
                    view.reads.insert(data.call.id.clone(), target);
                }
                None
            }
            EventKind::ToolResult(data) => {
                for result in &data.results {
                    let next = result
                        .result
                        .structured_content
                        .as_ref()
                        .and_then(|page| page.get("nextCursor"))
                        .and_then(serde_json::Value::as_u64);
                    view.tool_result(&result.call_id, result.result.is_error == Some(true), next);
                }
                None
            }
            EventKind::TurnStart(data) => {
                view.live_turn = Some(data.turn_id.clone());
                None
            }
            EventKind::TurnEnd(data) => {
                if view.live_turn.take().as_deref() != Some(data.turn_id.as_str()) {
                    return None;
                }
                if view.idle_notice == IdleNotice::None
                    || view.held.values().any(|mode| *mode != InputMode::Append)
                {
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
    /// The `nextCursor` of the line's latest successful read of `target`.
    async fn cursor(&self, target: &str) -> Result<Option<u64>> {
        if let Some(cursor) = self.view.lock().unwrap().cursors.get(target).copied() {
            return Ok(Some(cursor));
        }
        let equal = |path: &str, value: serde_json::Value| QueryFilter::Compare {
            left: QueryOperand::Field { path: path.into() },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        };
        let success = QueryFilter::Exists {
            condition: Box::new(QueryFilter::And {
                conditions: vec![
                    equal("kind", "step/tool-result".into()),
                    QueryFilter::Compare {
                        left: QueryOperand::Field {
                            path: "data.request_id".into(),
                        },
                        comparison: QueryComparison::Eq,
                        right: QueryOperand::Outer {
                            path: "data.request_id".into(),
                        },
                    },
                    QueryFilter::Any {
                        field: "data.results".into(),
                        condition: Box::new(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::Compare {
                                    left: QueryOperand::Element {
                                        path: "call_id".into(),
                                    },
                                    comparison: QueryComparison::Eq,
                                    right: QueryOperand::Outer {
                                        path: "data.call.id".into(),
                                    },
                                },
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::Compare {
                                        left: QueryOperand::Element {
                                            path: "result.isError".into(),
                                        },
                                        comparison: QueryComparison::Eq,
                                        right: QueryOperand::Value { value: true.into() },
                                    }),
                                },
                                QueryFilter::Present {
                                    value: QueryOperand::Element {
                                        path: "result.structuredContent.nextCursor".into(),
                                    },
                                },
                            ],
                        }),
                    },
                ],
            }),
        };
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: self.tree.head(&self.own),
            include_publishing: true,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        equal("kind", "step/tool-use".into()),
                        QueryFilter::TextCandidate {
                            field: "data".into(),
                            partition_by: vec![],
                            terms: vec!["branch".into(), target.into()],
                        },
                        equal("data.call.name", "branch".into()),
                        equal("data.call.input.action", "read".into()),
                        equal("data.call.input.branch", target.into()),
                        success,
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data.request_id".into(), "data.call.id".into()],
                },
                group_by: vec![],
                pick: Some(QueryPick::Last),
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: NonZeroU64::MIN,
                after: None,
            }),
        };
        let reply = self.tree.query(self.own.clone(), query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("read call identity")
        };
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let [
            QueryValue::Value { value: request },
            QueryValue::Value { value: call },
        ] = <[_; 2]>::try_from(row.values).unwrap()
        else {
            unreachable!("request and call identity")
        };
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                equal("kind", "step/tool-result".into()),
                equal("data.request_id", request),
                QueryFilter::Any {
                    field: "data.results".into(),
                    condition: Box::new(QueryFilter::Compare {
                        left: QueryOperand::Element {
                            path: "call_id".into(),
                        },
                        comparison: QueryComparison::Eq,
                        right: QueryOperand::Value {
                            value: call.clone(),
                        },
                    }),
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec![
                "data.results[].call_id".into(),
                "data.results[].result.structuredContent.nextCursor".into(),
            ],
        };
        let reply = self.tree.query(self.own.clone(), query).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("read cursor")
        };
        let row = rows.first().expect("successful read has a result");
        let [
            QueryValue::Elements { values: ids },
            QueryValue::Elements { values: cursors },
        ] = row.values.as_slice()
        else {
            unreachable!("result projections")
        };
        let cursor = ids.iter().zip(cursors).find_map(|(id, cursor)| {
            if matches!(id, QueryValue::Value { value } if value == &call) {
                let QueryValue::Value { value } = cursor else {
                    return None;
                };
                value.as_u64()
            } else {
                None
            }
        });
        if let Some(cursor) = cursor {
            self.view
                .lock()
                .unwrap()
                .cursors
                .insert(target.into(), cursor);
        }
        Ok(cursor)
    }
    /// The sequence this line was cut from its parent at; 0 for main.
    fn branch_point(&self) -> u64 {
        self.tree
            .info(self.own.clone())
            .ok()
            .and_then(|info| info.branch_point)
            .unwrap_or(0)
    }
    /// The sequence of the `branch/create` event on the parent that opened
    /// this line; every cut is at or below it.
    fn opened(&self) -> u64 {
        self.tree
            .info(self.own.clone())
            .ok()
            .and_then(|info| info.create_anchor)
            .unwrap_or(0)
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
        /// What woke the writer when its queue was empty.
        enum Wake {
            Queued,
            Parent,
            Lost,
        }
        let mut parent = self
            .resolved()
            .and_then(|(_, parent)| parent)
            .and_then(|parent| self.tree.turn_ends(&parent));
        loop {
            let next = self.wanted.lock().unwrap().pop_front();
            if let Some(decision) = next {
                if self.stop.is_cancelled() {
                    return;
                }
                let _ = self.write(decision).await;
                continue;
            }
            let wake = match parent.as_mut() {
                Some(signal) => {
                    tokio::select! {
                        _ = self.stop.cancelled() => return,
                        _ = self.changed.notified() => Wake::Queued,
                        changed = signal.changed() => {
                            if changed.is_ok() { Wake::Parent } else { Wake::Lost }
                        }
                    }
                }
                None => {
                    tokio::select! {
                        _ = self.stop.cancelled() => return,
                        _ = self.changed.notified() => Wake::Queued,
                    }
                }
            };
            match wake {
                Wake::Queued => {}
                Wake::Parent => self.want(Decision::Parent),
                Wake::Lost => parent = None,
            }
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
                self.roster_state().await?;
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
                self.roster_state().await?;
                let (text, retired) = self.roster();
                self.write_roster(text, retired, Some(branch)).await
            }
            Decision::Idle { turn, status } => {
                let Some(parent) = parent.filter(|_| role == LineRole::Tool) else {
                    return Ok(());
                };
                let needs_identity = self.view.lock().unwrap().roster.is_none();
                if needs_identity && let Some((id, text, retire)) = self.block(&line_key()).await? {
                    self.view
                        .lock()
                        .unwrap()
                        .input_block(&id, &text, &line_key(), retire);
                }
                let query = TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: None,
                    through: self.tree.head(&self.own),
                    include_publishing: true,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["input/message".into()],
                                },
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::Present {
                                        value: QueryOperand::Field {
                                            path: "data.handover".into(),
                                        },
                                    }),
                                },
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::Exists {
                                        condition: Box::new(QueryFilter::And {
                                            conditions: vec![
                                                QueryFilter::In {
                                                    value: QueryOperand::Field {
                                                        path: "kind".into(),
                                                    },
                                                    values: vec![
                                                        "input/consumed".into(),
                                                        "input/deleted".into(),
                                                    ],
                                                },
                                                QueryFilter::Compare {
                                                    left: QueryOperand::Field {
                                                        path: "data.input_id".into(),
                                                    },
                                                    comparison: QueryComparison::Eq,
                                                    right: QueryOperand::Outer {
                                                        path: "id".into(),
                                                    },
                                                },
                                            ],
                                        }),
                                    }),
                                },
                                QueryFilter::Or {
                                    conditions: vec![
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "data.mode".into(),
                                            },
                                            values: vec!["queue".into(), "steer".into()],
                                        },
                                        QueryFilter::And {
                                            conditions: vec![
                                                QueryFilter::Not {
                                                    condition: Box::new(QueryFilter::Present {
                                                        value: QueryOperand::Field {
                                                            path: "data.mode".into(),
                                                        },
                                                    }),
                                                },
                                                QueryFilter::Present {
                                                    value: QueryOperand::Field {
                                                        path: "data.user".into(),
                                                    },
                                                },
                                            ],
                                        },
                                    ],
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec!["id".into()],
                        },
                        group_by: vec![],
                        pick: Some(QueryPick::First),
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                };
                let QueryOutput::Rows { rows } =
                    self.tree.query(self.own.clone(), query).await?.result
                else {
                    unreachable!("current driving input")
                };
                if !rows.is_empty() && status == "completed" {
                    return Ok(());
                }

                {
                    let view = self.view.lock().unwrap();
                    if view.idle_notice == IdleNotice::None
                        || view.ending
                        || view.steered.as_deref() == Some(turn.as_str())
                    {
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
            Decision::Parent => {
                let Some(parent) = parent.filter(|_| role != LineRole::Main) else {
                    return Ok(());
                };
                let Some(head) = self.tree.head(&parent) else {
                    return Ok(());
                };
                let cut = self.branch_point();
                let opened = self.opened();
                let needs_parent = self.view.lock().unwrap().parent_input.is_none();
                if needs_parent && let Some((id, text, retire)) = self.block(&parent_key()).await? {
                    self.view
                        .lock()
                        .unwrap()
                        .input_block(&id, &text, &parent_key(), retire);
                }
                let parent_input = self.view.lock().unwrap().parent_input.clone();
                if let Some(id) = parent_input {
                    if self.parent_held(&id).await? {
                        return Ok(());
                    }
                    self.view.lock().unwrap().release(&id);
                }
                let cursor = self.cursor(&parent).await?;
                let (after, from) = {
                    let view = self.view.lock().unwrap();
                    if !parent_due(&view) || head <= view.parent_told {
                        return Ok(());
                    }
                    (cursor.unwrap_or(cut), parent_from(&view, &parent, opened))
                };
                if !self.tree.activity_after(&parent, from, head).await? {
                    return Ok(());
                }
                let body = format!(
                    "Line {parent} has new activity: it was at {head} when this was written, and you have read it to {after}. Read what is new with the branch tool (action read, branch \"{parent}\"); with no after, the read continues from {after}. Read it when what {parent} did changes what you do, and before your final message; do not page through all of {parent}'s work."
                );
                let block = crate::context_block::raw(
                    crate::context_block::element(
                        "dim-branch-parent",
                        &[
                            ("branch", &parent),
                            ("head", &head.to_string()),
                            ("after", &after.to_string()),
                        ],
                        &body,
                    ),
                    Some(parent_key()),
                );
                let data = InputMessageData {
                    blocks: vec![block],
                    mode: Some(InputMode::Append),
                    ..InputMessageData::default()
                };
                let written = self.tree.input(self.own.clone(), data).await?;
                let mut view = self.view.lock().unwrap();
                if view.parent_input.as_deref() != Some(written.id.as_str()) {
                    // The live fold may not have seen this input yet; a trigger
                    // that arrives first must still see it held.
                    view.held.insert(written.id.clone(), InputMode::Append);
                }
                view.parent_told = head;
                view.parent_input = Some(written.id.clone());
                Ok(())
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
            .map(|(id, child)| format!("- {id}: {}", crate::context_block::text(&child.purpose)))
            .collect::<Vec<_>>()
            .join("\n");
        (
            crate::context_block::element(
                "dim-branch-lines",
                &[("branch", &self.own)],
                &format!(
                    "Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it can read your work after that point, and you can read its.\n{listed}\n{}",
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
/// The line a `branch` read call names, when the call is a read.
fn read_target(call: &ToolCall) -> Option<String> {
    if call.name != "branch"
        || call.input.get("action").and_then(|value| value.as_str()) != Some("read")
    {
        return None;
    }
    call.input
        .get("branch")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}
/// The parent head a `<dim-branch-parent>` block reports.
fn parent_head(text: &str) -> Option<u64> {
    let rest = text.strip_prefix("<dim-branch-parent ")?;
    let start = rest.find("head=\"")? + "head=\"".len();
    let digits = &rest[start..];
    let end = digits.find('"')?;
    digits[..end].parse().ok()
}
/// Whether the line can be told about a parent move now: it is not ending and
/// no earlier parent block is still held (design §7.1.1).
fn parent_due(view: &LineView) -> bool {
    !view.ending
        && !view
            .parent_input
            .as_ref()
            .is_some_and(|id| view.held.contains_key(id))
}
/// The sequence the parent is checked after: what the line read, what the
/// last block told it, and its opening (design §7.1.1). Every cut is at or
/// below the opening, so a line that has not read starts there, and activity
/// from before the line existed is never news; the line reads it when it needs
/// it.
fn parent_from(view: &LineView, parent: &str, opened: u64) -> u64 {
    view.cursors
        .get(parent)
        .copied()
        .unwrap_or(opened)
        .max(view.parent_told)
        .max(opened)
}
/// Whether `event` is activity a line must hear about (design §7.1.1): an
/// operator input, or a tool call.
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
    #[serde(default, rename = "idleNotice")]
    idle_notice: IdleNotice,
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
        let has_idle_notice = call.input.contains_key("idleNotice");
        let input: BranchInput =
            serde_json::from_value(serde_json::Value::Object(call.input)).map_err(invalid)?;
        if input.action != "create" && has_idle_notice {
            return Err(invalid("idleNotice is only available for create"));
        }
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
            "read" => self.read(&own, input, &line).await,
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
                let through = parent.sequence();
                let mut query = TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: None,
                    through: Some(through),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["step/tool-use".into()],
                                },
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "data.turn_id".into(),
                                    },
                                    values: vec![context.turn_id.clone().into()],
                                },
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "data.request_id".into(),
                                    },
                                    values: vec![context.request_id.clone().into()],
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec!["position.sequence".into()],
                        },
                        group_by: vec![],
                        pick: Some(QueryPick::First),
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                };
                let reply = parent
                    .query(query.clone())
                    .await
                    .map_err(|error| HostError::failed(error.to_string()))?;
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("field selection returns rows")
                };
                let Some(row) = rows.first() else {
                    return Err(HostError::failed(
                        "branch tool call is absent from its owning timeline",
                    ));
                };
                let [QueryValue::Value { value: sequence }] = row.values.as_slice() else {
                    unreachable!("tool use has a sequence")
                };
                query.through = Some(sequence.as_u64().expect("event sequence is unsigned") - 1);
                let Some(QueryFilter::And { conditions }) = &mut query.query.filter else {
                    unreachable!()
                };
                conditions[0] = QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec!["step/model-start".into()],
                };
                query.query.pick = Some(QueryPick::Last);
                let reply = parent
                    .query(query)
                    .await
                    .map_err(|error| HostError::failed(error.to_string()))?;
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("field selection returns rows")
                };
                let Some(row) = rows.first() else {
                    return Err(HostError::failed(
                        "branch tool batch has no owning model start",
                    ));
                };
                let [QueryValue::Value { value: sequence }] = row.values.as_slice() else {
                    unreachable!("model start has a sequence")
                };
                sequence.as_u64().expect("event sequence is unsigned") - 1
            }
        };
        let branch = self
            .tree
            .branch_with_idle_notice(
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
                input.idle_notice,
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
    /// One page of a line's committed history, open or ended (design §6).
    async fn read(
        &self,
        own: &OwnLine,
        input: BranchInput,
        line: &BranchLine,
    ) -> std::result::Result<ToolResult, HostError> {
        let id = input
            .branch
            .ok_or_else(|| invalid("read requires branch"))?
            .clone();
        let Some(target) = self.line(&id) else {
            return Ok(self.not_kin());
        };
        let permitted = match own.role {
            LineRole::Main => kin(own, &target),
            LineRole::Tool | LineRole::Host => own.parent.as_deref() == Some(target.id.as_str()),
        };
        if !permitted {
            return Ok(self.not_kin());
        }
        if own.role == LineRole::Main && matches!(target.source, Some(BranchSource::Host { .. })) {
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
        // main reads from a line's start; a line reads from its cut. Without
        // `after`, a read continues after the reader's last read of the
        // target, and a parent read never goes below the cut (design §6).
        let cut = if own.role == LineRole::Main {
            0
        } else {
            self.tree
                .info(own.id.clone())
                .ok()
                .and_then(|info| info.branch_point)
                .unwrap_or(0)
        };
        let after = match input.after {
            Some(after) if own.role == LineRole::Main => after,
            Some(after) => after.max(cut),
            None => line
                .cursor(&target.id)
                .await
                .map_err(|error| HostError::failed(error.to_string()))?
                .unwrap_or(cut),
        };
        let head = self
            .tree
            .head(&target.id)
            .ok_or_else(|| HostError::failed(format!("unknown line {}", target.id)))?;
        if after > head {
            return Err(HostError::failed(format!(
                "after {after} is past line {}'s head {head}; a read starts at or below {head}",
                target.id
            )));
        }
        let next = self
            .tree
            .page_end(
                &target.id,
                after,
                NonZeroU64::new(limit as u64).unwrap(),
                head,
            )
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let items = if after == next {
            Vec::new()
        } else {
            line_read_items(&self.tree, &target.id, after, next)
                .await
                .map_err(|error| HostError::failed(error.to_string()))?
        };
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
            "You can message or read only the lines you opened and the line you were cut from."
                .into(),
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
enum LineRead {
    Input(InputMessageData),
    Text(TextData),
    Start(ToolUseStartData),
    End(TurnEndData),
    Calls {
        request: String,
        calls: Vec<(String, String)>,
    },
    Results {
        request: String,
        results: Vec<(String, bool, Option<Vec<ToolEffect>>)>,
    },
}

async fn line_read_items(
    tree: &TreeCommands,
    branch: &str,
    after: u64,
    through: u64,
) -> Result<Vec<serde_json::Value>> {
    let mut facts = Vec::new();
    let selections: [(&[&str], &[&str]); 3] = [
        (
            &[
                "input/message",
                "step/text",
                "step/tool-use-start",
                "turn/end",
            ],
            &["position.sequence", "kind", "data"],
        ),
        (
            &["step/tool-call"],
            &[
                "position.sequence",
                "data.request_id",
                "data.calls[].id",
                "data.calls[].name",
            ],
        ),
        (
            &["step/tool-result"],
            &[
                "position.sequence",
                "data.request_id",
                "data.results[].call_id",
                "data.results[].result.is_error",
                "data.results[].result.effects",
            ],
        ),
    ];
    for (index, (kinds, fields)) in selections.into_iter().enumerate() {
        let reply = tree
            .query(
                branch.into(),
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: Some(TimelinePosition {
                        branch_id: branch.into(),
                        sequence: after + 1,
                    }),
                    through: Some(through),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: kinds.iter().map(|kind| serde_json::json!(kind)).collect(),
                        }),
                        select: QuerySelect::Fields {
                            fields: fields.iter().map(|field| (*field).into()).collect(),
                        },
                        group_by: vec![],
                        pick: None,
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::new(through - after).unwrap(),
                        after: None,
                    }),
                },
            )
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("line read fields")
        };
        for row in rows {
            let QueryValue::Value { value: sequence } = &row.values[0] else {
                unreachable!("event sequence")
            };
            let sequence = sequence.as_u64().expect("event sequence");
            let fact = match (index, row.values.as_slice()) {
                (
                    0,
                    [
                        _,
                        QueryValue::Value {
                            value: serde_json::Value::String(kind),
                        },
                        QueryValue::Value { value: data },
                    ],
                ) => match kind.as_str() {
                    "input/message" => {
                        LineRead::Input(serde_json::from_value(data.clone()).expect("input fields"))
                    }
                    "step/text" => {
                        LineRead::Text(serde_json::from_value(data.clone()).expect("text fields"))
                    }
                    "step/tool-use-start" => LineRead::Start(
                        serde_json::from_value(data.clone()).expect("tool start fields"),
                    ),
                    "turn/end" => LineRead::End(
                        serde_json::from_value(data.clone()).expect("turn end fields"),
                    ),
                    _ => unreachable!("selected line read kind"),
                },
                (
                    1,
                    [
                        _,
                        QueryValue::Value {
                            value: serde_json::Value::String(request),
                        },
                        QueryValue::Elements { values: ids },
                        QueryValue::Elements { values: names },
                    ],
                ) => {
                    assert_eq!(
                        ids.len(),
                        names.len(),
                        "call projections preserve array positions"
                    );
                    let calls = ids
                        .iter()
                        .zip(names)
                        .map(|(id, name)| {
                            let (
                                QueryValue::Value {
                                    value: serde_json::Value::String(id),
                                },
                                QueryValue::Value {
                                    value: serde_json::Value::String(name),
                                },
                            ) = (id, name)
                            else {
                                unreachable!("call identity fields")
                            };
                            (id.clone(), name.clone())
                        })
                        .collect();
                    LineRead::Calls {
                        request: request.clone(),
                        calls,
                    }
                }
                (
                    2,
                    [
                        _,
                        QueryValue::Value {
                            value: serde_json::Value::String(request),
                        },
                        QueryValue::Elements { values: ids },
                        QueryValue::Elements { values: errors },
                        QueryValue::Elements { values: effects },
                    ],
                ) => {
                    assert_eq!(
                        ids.len(),
                        errors.len(),
                        "result projections preserve array positions"
                    );
                    assert_eq!(
                        ids.len(),
                        effects.len(),
                        "result projections preserve array positions"
                    );
                    let results = ids
                        .iter()
                        .zip(errors)
                        .zip(effects)
                        .map(|((id, error), effects)| {
                            let QueryValue::Value {
                                value: serde_json::Value::String(id),
                            } = id
                            else {
                                unreachable!("result identity field")
                            };
                            let error = match error {
                                QueryValue::Value {
                                    value: serde_json::Value::Bool(error),
                                } => *error,
                                QueryValue::Missing => false,
                                _ => unreachable!("result error flag"),
                            };
                            let effects: Option<Vec<ToolEffect>> = match effects {
                                QueryValue::Missing => None,
                                QueryValue::Value { value: effects } => Some(
                                    serde_json::from_value(effects.clone())
                                        .expect("declared tool effects"),
                                ),
                                _ => unreachable!("declared effects are an array"),
                            };
                            (
                                id.clone(),
                                error,
                                effects
                                    .filter(|effects| !effects.is_empty())
                                    .map(|effects| {
                                        effects.iter().map(ToolEffect::shortened).collect()
                                    }),
                            )
                        })
                        .collect();
                    LineRead::Results {
                        request: request.clone(),
                        results,
                    }
                }
                _ => unreachable!("line read projection contract"),
            };
            facts.push((sequence, fact));
        }
    }
    facts.sort_by_key(|(sequence, _)| *sequence);
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut text_request: Option<String> = None;
    let mut previous = after;
    let mut names = BTreeMap::<(String, String), String>::new();
    for (sequence, fact) in facts {
        let mut merged = None;
        match fact {
            LineRead::Input(data) => items.push(serde_json::json!({"sequence":sequence,"type":"input","mode":data.effective_mode(),"user":data.user,"blocks":data.blocks})),
            LineRead::Text(data) => {
                if text_request.as_deref()==Some(data.request_id.as_str()) && sequence==previous+1
                    && let Some(text)=items.last_mut().and_then(|item|item.get_mut("text")) {
                    *text=serde_json::json!(format!("{}{}",text.as_str().unwrap(),data.delta));
                } else { items.push(serde_json::json!({"sequence":sequence,"type":"text","text":data.delta})); }
                merged=Some(data.request_id);
            },
            LineRead::Start(data)=>{ names.insert((data.request_id,data.call_id),data.name); },
            LineRead::Calls { request,calls }=>for (id,name) in calls { names.insert((request.clone(),id.clone()),name.clone()); items.push(serde_json::json!({"sequence":sequence,"type":"tool_call","callId":id,"name":name})); },
            LineRead::Results { request,results }=>for (id,error,effects) in results {
                let mut item=serde_json::json!({"sequence":sequence,"type":"tool_result","callId":id,"status":if error {"error"} else {"ok"}});
                if let Some(name)=names.get(&(request.clone(),id)) { item["name"]=serde_json::json!(name); }
                if let Some(effects)=effects { item["effects"]=serde_json::json!(effects); }
                items.push(item);
            },
            LineRead::End(data)=>{
                let mut item=serde_json::json!({"sequence":sequence,"type":"turn_end"});
                match data.outcome {
                    TurnOutcome::Completed {..}=>item["status"]=serde_json::json!("completed"),
                    TurnOutcome::Failed { error }=>{item["status"]=serde_json::json!("failed");item["reason"]=serde_json::json!(error);},
                    TurnOutcome::Cancelled { reason }=>{item["status"]=serde_json::json!("cancelled");item["reason"]=serde_json::json!(reason);},
                }
                items.push(item);
            },
        }
        previous = sequence;
        text_request = merged;
    }
    Ok(items)
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
fn parent_key() -> String {
    "branch:parent".into()
}

fn first_input(parent: &str, data: &BranchCreateData, idle_notice: IdleNotice) -> InputMessageData {
    let (blocks, user) = match data.source {
        BranchSource::Tool { .. } => (
            vec![tool_line_identity(
                &data.branch,
                parent,
                &data.prompt,
                idle_notice,
            )],
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

/// The block a nested line gets at open: it keeps the parent key live and
/// names the line that opened it at the cut, so the block it inherited, which
/// names a line it cannot read, is no longer its latest (§7.1.1).
fn parent_open_block(parent: &str, cut: u64) -> InputBlock {
    crate::context_block::raw(
        crate::context_block::element(
            "dim-branch-parent",
            &[
                ("branch", parent),
                ("head", &cut.to_string()),
                ("after", &cut.to_string()),
            ],
            &format!(
                "Line {parent} has no new activity since you were cut from it at {cut}. Parent blocks above this point were written for {parent} and are not yours."
            ),
        ),
        Some(parent_key()),
    )
}

fn tool_line_identity(
    line: &str,
    parent: &str,
    prompt: &str,
    idle_notice: IdleNotice,
) -> InputBlock {
    let prompt = crate::context_block::text(prompt);
    let body = [
        format!("You are line {line} of this session. Line {parent} opened you at this point of its history to work in parallel with it. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line. {parent} keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to {parent}, such as its list of open lines, are {parent}'s, not yours."),
        String::new(),
        format!("Your purpose, as {parent} wrote it:"),
        prompt,
        String::new(),
        format!("How you work with {parent}:"),
        format!("- Work on this purpose only. You cannot open lines; if more parallel work is needed, tell {parent}."),
        format!("- Read {parent} with the branch tool (action read, branch \"{parent}\") when what {parent} did or decided since your cut changes your work; the parent block reports when {parent} has moved. Message {parent} only for what it must act on: a decision that changes its work, a blocker only the user can settle, a conflict with another line, or a change to your purpose. Use mode \"steer\" for what {parent} must act on now, \"append\" for what {parent} should see at its next step. Do not send {parent} a progress report it can read, and do not page through {parent}'s whole stream: read the page that answers your question, then act."),
        format!("- You share the workspace with {parent} and its other lines. Change only the files {parent} gave you; before changing any other file, tell {parent} which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell {parent} before you continue."),
        "- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.".to_string(),
        format!("- When your purpose is done, or cannot be done, call the branch tool with action \"end\" and content saying what you found, did, changed and left undone. If you already told {parent} that in a message, your end content carries only what is new. Read {parent} once more before you end if the parent block says {parent} has moved, so your last word still holds. That is your last action; {parent} receives it."),
    ]
    .join("\n");
    let mut attrs = vec![("branch", line), ("parent", parent), ("source", "tool")];
    if idle_notice == IdleNotice::None {
        attrs.push((QUIET_IDLE_ATTRIBUTE, "none"));
    }
    crate::context_block::raw(
        crate::context_block::element("dim-branch-line", &attrs, &body),
        Some(line_key()),
    )
}

fn host_line_identity(line: &str, parent: &str) -> InputBlock {
    crate::context_block::raw(
        crate::context_block::element(
            "dim-branch-line",
            &[("branch", line), ("parent", parent), ("source", "host")],
            &format!(
                "You are line {line} of this session, opened from line {parent} at this point of its history. You are the same agent as {parent}: everything above is your own history, and you continue from here on your own line, in parallel with {parent}. You can read {parent}'s work after this point. Messages above that were addressed to {parent} are {parent}'s, not yours. The message after this one says what this line is for."
            ),
        ),
        Some(line_key()),
    )
}

/// The `branch` tool's description: its policy sentences are the coordination
/// text for `main` ([timeline branches §12.4](../../../docs/design/timeline-branches.md#124-texts-and-their-classes)),
/// replaceable at [`BranchToolConfig`].
const BRANCH_TOOL_DESCRIPTION: &str = "Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed by reading each other's work and messaging what the other must act on. On main you coordinate: answer a line's steer and act on what it asks for; leave a line's part to that line instead of redoing it; read a line's work instead of waiting for a report.\n- create (main only): open a line to work on prompt in parallel. Open one line per part that can go on at the same time as the rest — separate files, a separate question, or a second approach — instead of doing those parts one after another yourself; not for a single quick edit, nor when your next step needs its result. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Returns the line's id.\n- message: send content to a line you opened, or to the line that opened you. Send a decision, a blocker, or a conflict, not a progress report the receiver can read. Mode \"append\" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use \"steer\" when the receiver must act now or is idle.\n- read: show a page of the activity of the line you were cut from, or of a line you opened, also after it ended. With no after, a read continues after your last read of that line: from your cut for the line you were cut from, and from its first event for a line you opened. Pass after to read from another point. Do not page through a whole line.\n- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.\nA line reads the line it was cut from; main reads the lines it opened. Messages pass only between main and the lines it opened.";

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
                "idleNotice":{"type":"string","enum":["steer","none"],"description":"create: automatic notice to the parent after the line goes idle. Use none for a line that should wait quietly. Defaults to steer."},
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
            parent_call_id: None,
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

    /// A read result carrying `nextCursor`, as the branch tool returns it.
    fn page(sequence: u64, id: &str, next: Option<u64>, is_error: bool) -> Arc<AgentEvent> {
        let mut result = if is_error {
            ToolResult::error("refused")
        } else {
            ToolResult::text(format!(
                r#"{{"items":[],"nextCursor":{next:?},"done":false}}"#
            ))
        };
        if let Some(next) = next {
            result.structured_content = json!({"items": [], "nextCursor": next, "done": false})
                .as_object()
                .cloned();
        }
        event(
            sequence,
            EventKind::ToolResult(ToolResultData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_event_id: Some("call-event".into()),
                results: vec![ToolCallResult {
                    call_id: id.into(),
                    result,
                }],
            }),
        )
    }

    #[test]
    fn the_fold_keeps_the_next_cursor_of_each_successful_read() {
        let mut view = line();
        BranchLine::fold(
            &mut view,
            &call(
                1,
                "call",
                "branch",
                json!({"action":"read","branch":"main"}),
            ),
        );
        assert_eq!(view.reads.get("call").map(String::as_str), Some("main"));
        BranchLine::fold(&mut view, &page(2, "call", Some(42), false));
        assert_eq!(view.cursors.get("main"), Some(&42));
        assert!(view.reads.is_empty());
        BranchLine::fold(
            &mut view,
            &call(
                3,
                "failed",
                "branch",
                json!({"action":"read","branch":"main"}),
            ),
        );
        BranchLine::fold(&mut view, &page(4, "failed", Some(99), true));
        assert_eq!(
            view.cursors.get("main"),
            Some(&42),
            "a refused read moves no cursor"
        );
        BranchLine::fold(
            &mut view,
            &call(
                5,
                "elsewhere",
                "branch",
                json!({"action":"read","branch":"other"}),
            ),
        );
        BranchLine::fold(&mut view, &page(6, "elsewhere", Some(7), false));
        assert_eq!(view.cursors.get("other"), Some(&7));
        assert_eq!(view.cursors.get("main"), Some(&42), "cursors are per line");
    }

    #[test]
    fn a_parent_block_starts_after_the_read_cursor_the_last_block_and_the_opening() {
        let mut view = line();
        view.cursors.insert("main".into(), 5);
        assert_eq!(parent_from(&view, "main", 1), 5);
        assert_eq!(
            parent_from(&view, "other", 8),
            8,
            "activity before the opening is not news"
        );
        view.parent_told = 6;
        assert_eq!(parent_from(&view, "main", 1), 6);
        assert_eq!(parent_from(&view, "main", 9), 9, "the opening is a floor");
        let text = crate::context_block::element(
            "dim-branch-parent",
            &[("branch", "main"), ("head", "6"), ("after", "5")],
            "Line main has new activity.",
        );
        BranchLine::fold(
            &mut view,
            &event(
                7,
                EventKind::InputMessage(InputMessageData {
                    blocks: vec![crate::context_block::raw(text, Some(parent_key()))],
                    mode: Some(InputMode::Append),
                    ..InputMessageData::default()
                }),
            ),
        );
        assert_eq!(view.parent_told, 6, "the block's head is folded");
        assert_eq!(view.parent_input.as_deref(), Some("event-7"));
        assert!(!parent_due(&view), "a held block is not written over");
        view.held.clear();
        assert!(parent_due(&view));
        view.ending = true;
        assert!(!parent_due(&view), "an ending line gets no block");
    }
}
