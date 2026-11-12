//! Goal's branch part (design §1, A.6, Pursuit §4): owns this branch's fold,
//! decides continuation from consumed state only, and writes through owned
//! tasks. On a branch Goal created for an evaluation it takes the evaluator
//! role instead (Pursuit §4.2).

use crate::fold::{self, Conclusion, ConclusionAction, Decision, Fold, TurnEnd};
use crate::text::{self, GoalTexts, Live};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    AgentError, BoxFuture, BranchLifecycle, BranchOptions, Dispatcher, Names, QueryComparison,
    QueryFilter, QueryOperand, QueryOrder, QueryOrderKey, QueryOutput, QueryPage, QueryPick,
    QuerySelect, QuerySpec, QueryValue, SessionLifecycle, Timeline, TimelineIdentity, TimelineKey,
    TimelineQuery, TimelineQueryDirection, TimelineQueryScope,
};
use dim_agent_types::{
    AgentEvent, BranchSource, EventDraft, EventKind, ExecStatus, GoalRead, InputBlock,
    InputMessageData, InterruptScope, TurnEndData, TurnOutcome,
};
use dim_app_server::settings::{SettingsScope, SettingsService};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) mod recovery;
mod restore;

const SOURCE: &str = "goal";

/// One change Goal's callers ask for (D3). The branch part writes its trigger
/// and the record that states it as one atomic batch, so the first model
/// request a change drives already carries its record (R5).
#[derive(Clone)]
pub(crate) enum Change {
    Set {
        objective: String,
        judge: text::Judge,
    },
    Edit {
        objective: String,
    },
    Pause,
    Resume,
    Clear,
    Report {
        completed: bool,
        summary: String,
    },
}

impl Change {
    /// The trigger input for `branch`; each keeps the mode it had when the
    /// Session part wrote it alone (A.5).
    fn trigger(&self, branch: &str) -> InputMessageData {
        match self {
            Self::Set { objective, judge } => text::set(branch, objective, *judge),
            Self::Edit { objective } => text::edit(branch, objective),
            Self::Pause => text::pause(branch),
            Self::Resume => text::resume(branch),
            Self::Clear => text::clear(branch),
            Self::Report { completed, summary } => text::report(
                branch,
                if *completed { "completed" } else { "blocked" },
                summary,
            ),
        }
    }
}

/// The loaded branch Owners, by Session and branch identity. The process plugin hands the same
/// table to its Session part and its branch parts, so a `session.goal.*` call
/// or the `goal_update` tool reaches the branch's Owner, which composes the
/// record, without a service (ADR 002 D3).
#[derive(Clone, Default)]
pub(crate) struct Changes {
    owners: Arc<Mutex<BTreeMap<TimelineKey, Weak<Owner>>>>,
}

impl Changes {
    pub(crate) async fn current(&self, key: &TimelineKey) -> Result<GoalRead, AgentError> {
        let owner = self
            .owners
            .lock()
            .unwrap()
            .get(key)
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::Reloading("Goal".into()))?;
        owner.current().await
    }
    /// Writes `change` on `branch` through its Owner and returns the
    /// trigger's sequence.
    pub(crate) async fn change(
        &self,
        key: &TimelineKey,
        change: Change,
    ) -> Result<u64, AgentError> {
        let owner = self
            .owners
            .lock()
            .unwrap()
            .get(key)
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::Reloading("Goal".into()))?;
        owner.change(change).await
    }
    /// Registers `owner` for its branch; the returned handle withdraws it.
    fn register(&self, key: TimelineKey, owner: &Arc<Owner>) -> Weak<Owner> {
        let weak = Arc::downgrade(owner);
        self.owners.lock().unwrap().insert(key, weak.clone());
        weak
    }
    fn withdraw(&self, key: &TimelineKey, owner: &Weak<Owner>) {
        let mut owners = self.owners.lock().unwrap();
        if owners
            .get(key)
            .is_some_and(|entry| Weak::ptr_eq(entry, owner))
        {
            owners.remove(key);
        }
    }
}

/// The facts the fold consumes (A.6).
pub(crate) const KEYS: &[&str] = &[
    "branch/start",
    "session/forked",
    "branch/create",
    "branch/create-failed",
    "branch/ended",
    "input/message",
    "input/consumed",
    "input/deleted",
    "turn/start",
    "turn/end",
    "stop/interrupt",
    "exec/started",
    "exec/ended",
];

/// Inherited `input/message` facts one page of the new-branch check keeps.
const INHERITED_PAGE: u64 = 64;
/// Facts one page of the reply read keeps.
const REPLY_PAGE: u64 = 256;

/// The Settings document Goal declares.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    /// Absent means no limit; an explicit `null` is not a value (A4).
    #[serde(default, deserialize_with = "optional_non_null")]
    max_continuations: Option<u64>,
}

fn optional_non_null<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Some)
}

pub struct GoalBranch {
    dispatcher: Key<Dispatcher>,
    timeline: Key<Timeline>,
    tree: Key<TreeCommands>,
    settings: Key<SettingsService>,
    identity: Key<TimelineIdentity>,
    phase: Key<BranchLifecycle>,
    session: Key<SessionLifecycle>,
    texts: Arc<GoalTexts>,
    changes: Changes,
}

impl GoalBranch {
    pub fn new(names: &Names<'_>, texts: Arc<GoalTexts>, changes: Changes) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            timeline: names.key()?,
            tree: names.key()?,
            settings: names.key()?,
            identity: names.key()?,
            phase: names.key()?,
            session: names.key()?,
            texts,
            changes,
        })
    }
}

impl DynPlugin for GoalBranch {
    fn name(&self) -> &str {
        "GoalBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.timeline.erase(),
            self.tree.erase(),
            self.settings.erase(),
            self.identity.erase(),
            self.phase.erase(),
            self.session.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let tree = ctx.require_at(&self.tree)?;
            let settings = ctx.require_at(&self.settings)?;
            let identity = ctx.require_at(&self.identity)?;
            let phase = ctx.require_at(&self.phase)?;
            let session = ctx.require_at(&self.session)?;
            let stop = CancellationToken::new();
            let (resolved, _) = watch::channel(false);
            let (stepped, _) = watch::channel(0);
            let (ready, _) = watch::channel(false);
            let (write_status, _) = watch::channel(Ok(()));
            let owner = Arc::new(Owner {
                branch_id: identity.branch_id.clone(),
                timeline,
                dispatcher: dispatcher.clone(),
                tree,
                settings,
                texts: self.texts.clone(),
                phase: phase.clone(),
                session: session.clone(),
                stop: stop.clone(),
                tasks: TaskTracker::new(),
                work: TaskTracker::new(),
                resolved,
                stepped,
                ready,
                write_status,
                state: Mutex::new(State::default()),
                write: tokio::sync::Mutex::new(()),
            });
            let key = TimelineKey {
                session_id: identity.session_id.clone(),
                branch_id: identity.branch_id.clone(),
            };
            let registered = self.changes.register(key.clone(), &owner);
            let cleanup = owner.clone();
            let changes = self.changes.clone();
            ctx.effect(move || async move {
                cleanup.stop.cancel();
                cleanup.work.close();
                cleanup.work.wait().await;
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
                cleanup.pause_closing().await?;
                changes.withdraw(&key, &registered);
                Ok(())
            })?;
            let closing = owner.clone();
            let lifecycle = phase.clone();
            owner.tasks.spawn(async move {
                tokio::select! {
                    _ = lifecycle.closing() => {
                        closing.stop.cancel();
                        closing.state.lock().unwrap().decisions.cancel();
                        closing.work.close();
                        closing.work.wait().await;
                        if let Err(error)=closing.pause_closing().await {closing.dispatcher.fail(error);}
                    },
                    _ = closing.stop.cancelled() => {},
                }
            });
            // Who this branch is, and whether it is an evaluator line, is read
            // once and before any decision: every decision waits for it.
            let resolve = owner.clone();
            owner.tasks.spawn(async move {
                let active = tokio::select! {
                    active = resolve.phase.activated() => active,
                    _ = resolve.stop.cancelled() => false,
                };
                if active
                    && !resolve.stopped()
                    && let Ok(info) = resolve.tree.info(resolve.branch_id.clone())
                {
                    let evaluation = match &info.source {
                        Some(BranchSource::Host { operation_id }) => {
                            fold::evaluation_of_operation(operation_id)
                        }
                        _ => None,
                    };
                    if let (Some(parent), Some(evaluation)) = (info.parent.clone(), evaluation) {
                        resolve.state.lock().unwrap().role = Some(Role {
                            branch: parent,
                            evaluation,
                        });
                    }
                }
                resolve.resolved.send_replace(true);
            });
            let rebuilding = owner.clone();
            let consuming = owner.clone();
            dispatcher
                .consume(
                    ctx,
                    "Goal".into(),
                    KEYS.iter()
                        .copied()
                        .chain([recovery::KEY])
                        .map(str::to_owned)
                        .collect(),
                    Arc::new(move || {
                        let owner = rebuilding.clone();
                        Box::pin(async move { owner.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        let owner = consuming.clone();
                        Box::pin(async move { owner.ingest(&event, true) })
                    }),
                )
                .await?;
            // The rebuild has returned and every fact it held is folded, so a
            // change can compose from the complete fold (R6.2).
            owner.ready.send_replace(true);
            Ok(Effect::Done)
        })
    }
}

#[derive(Clone)]
struct Role {
    /// The branch the evaluator reports to.
    branch: String,
    evaluation: u64,
}

/// What a request for a key whose work is already in flight asks for when
/// that work finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retry {
    /// A turn-end request: take the turn-end decision again (A.4).
    TurnEnd,
    /// A settle request: take a deferred conclusion or the turn-end decision
    /// again (Pursuit §5).
    Settle,
}

#[derive(Default)]
struct State {
    fold: Fold,
    source_through: u64,
    decisions: CancellationToken,
    decision_tasks: TaskTracker,
    terminating: bool,
    /// The opening event the fold started at.
    start: Option<u64>,
    role: Option<Role>,
    /// The decision keys whose work is running: each runs once at a time.
    in_flight: HashSet<String>,
    /// What a key asked for while its work was running waits for.
    retry: HashMap<String, Retry>,
    /// The `after` values of the state records Goal has written: a second pass
    /// that runs before the fold consumes a write must not state it again.
    written: std::collections::BTreeSet<u64>,
    /// The highest `after` a state record was composed with. Records are
    /// ordered by `after`, not by their commit order (R4): a record naming an
    /// older state is not composed once a newer one exists.
    recorded: u64,
    /// The sequence of the newest Goal input this generation committed on the
    /// branch. A change composes its record only once the fold has stepped it
    /// (R5).
    last_write: u64,
}

struct Owner {
    branch_id: String,
    timeline: Arc<Timeline>,
    dispatcher: Arc<Dispatcher>,
    tree: Arc<TreeCommands>,
    settings: Arc<SettingsService>,
    texts: Arc<GoalTexts>,
    phase: Arc<BranchLifecycle>,
    session: Arc<SessionLifecycle>,
    stop: CancellationToken,
    tasks: TaskTracker,
    work: TaskTracker,
    /// Set once the branch's identity is read; every decision waits for it.
    resolved: watch::Sender<bool>,
    /// The sequence of the newest event the fold has stepped, so a change can
    /// wait for the fold's position before composing its record (R5).
    stepped: watch::Sender<u64>,
    /// Set once the rebuild has returned and the facts it held are folded
    /// (R6.2): a change composes only from that complete fold.
    ready: watch::Sender<bool>,
    write_status: watch::Sender<Result<(), AgentError>>,
    state: Mutex<State>,
    /// Serializes state records: each is composed from the fold as it stands
    /// when the lock is taken and commits before the next one is composed, so
    /// the newest record is the one that states the newest state (D3).
    write: tokio::sync::Mutex<()>,
}

impl Owner {
    fn stopped(&self) -> bool {
        self.stop.is_cancelled()
    }

    async fn resolved(&self) {
        let mut resolved = self.resolved.subscribe();
        while !*resolved.borrow() {
            if resolved.changed().await.is_err() {
                return;
            }
        }
    }

    /// Commits one Goal input on this branch and remembers it as the branch's
    /// newest Goal write (R5).
    async fn write(&self, input: InputMessageData) -> Result<Arc<AgentEvent>, AgentError> {
        let event = self.tree.input(self.branch_id.clone(), input).await?;
        let mut state = self.state.lock().unwrap();
        state.last_write = state.last_write.max(event.sequence);
        drop(state);
        Ok(event)
    }

    /// Waits until this generation's rebuild has returned and folded the facts
    /// it held (R6.2): before that, the fold is partial and a record composed
    /// from it would state a state the branch does not have. The batch is
    /// written through the tree, which resolves a branch only once its build
    /// returned (`TimelineTree::get`,
    /// crates/dim-agent-capabilities/src/timeline_tree.rs:118); the branch's
    /// own activation is that signal (R7.2). Bounded by `stop` like every
    /// wait; a generation that ends first fails the call.
    async fn await_ready(&self) -> Result<(), AgentError> {
        let mut ready = self.ready.subscribe();
        loop {
            if *ready.borrow_and_update() {
                break;
            }
            tokio::select! {
                result = ready.changed() => {
                    if result.is_err() {
                        return Err(AgentError::Reloading("Goal".into()));
                    }
                }
                _ = self.stop.cancelled() => {
                    return Err(AgentError::Closed(self.branch_id.clone()));
                }
            }
        }
        // The batch is written through the tree, which resolves a branch only
        // once its build returned; the branch's own activation is that signal.
        // A branch that closes without starting never publishes, so it ends
        // the call like `stop`.
        tokio::select! {
            active = self.phase.activated() => {
                if !active {
                    return Err(AgentError::Closed(self.branch_id.clone()));
                }
            }
            _ = self.stop.cancelled() => {
                return Err(AgentError::Closed(self.branch_id.clone()));
            }
        }
        Ok(())
    }

    /// Waits until the fold has stepped this generation's newest Goal write,
    /// so the record composed next is composed from a fold that holds it (R5).
    /// The fold is stepped by live delivery, never by this wait.
    async fn await_fold(&self) -> Result<(), AgentError> {
        let target = self.state.lock().unwrap().last_write;
        if target == 0 {
            return Ok(());
        }
        let mut stepped = self.stepped.subscribe();
        loop {
            if *stepped.borrow_and_update() >= target {
                return Ok(());
            }
            tokio::select! {
                result = stepped.changed() => {
                    if result.is_err() {
                        return Err(AgentError::Reloading("Goal".into()));
                    }
                }
                _ = self.stop.cancelled() => {
                    return Err(AgentError::Closed(self.branch_id.clone()));
                }
            }
        }
    }

    /// Writes `change` for the Session part and the `goal_update` tool: the
    /// state record and the trigger as one atomic batch, the record first, so
    /// the trigger's turn starts with the record already ahead of it (R5).
    async fn change(self: &Arc<Self>, change: Change) -> Result<u64, AgentError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let owner = self.clone();
        self.tasks.spawn(async move {
            let _ = reply.send(owner.change_inner(change).await);
        });
        result
            .await
            .map_err(|_| AgentError::Closed("Goal".into()))?
    }

    async fn change_inner(self: &Arc<Self>, change: Change) -> Result<u64, AgentError> {
        self.await_ready().await?;
        let _write = self.write_guard().await?;
        let settled = self.settle_decisions().await;
        if let Err(error) = settled {
            self.state.lock().unwrap().terminating = false;
            return Err(error);
        }
        let result = async {
            if matches!(change, Change::Set { .. } | Change::Resume) {
                self.load_current_work().await?;
            }
            let trigger = change.trigger(&self.branch_id);
            let (objective, state, judge, live, decisions) = {
                let lock = self.state.lock().unwrap();
                let (attrs, op) = fold::change_of(&trigger.blocks)
                    .expect("a change carries its own trigger marker");
                let (objective, mut state, mut judge) = lock.fold.after_change(op, &attrs);
                if objective.is_none() {
                    state = text::State::None;
                    judge = text::Judge::Model;
                }
                (
                    objective,
                    state,
                    judge,
                    lock.fold.live,
                    lock.decisions.clone(),
                )
            };
            let trigger_id = trigger
                .submission_id
                .clone()
                .expect("Goal inputs carry a submission id");
            let operation = match &change {
                Change::Set { .. } => "set",
                Change::Edit { .. } => "edit",
                Change::Pause => "pause",
                Change::Resume => "resume",
                Change::Clear => "clear",
                Change::Report {
                    completed: true, ..
                } => "completed",
                Change::Report {
                    completed: false, ..
                } => "blocked",
            };
            let business = recovery::BusinessState::new(
                operation,
                objective.clone(),
                state.clone(),
                judge,
                None,
            );
            let record = text::state(
                &self.branch_id,
                objective.as_deref(),
                state,
                judge,
                text::Answered::Trigger(&trigger_id),
                live,
                &self.texts,
            );
            if decisions.is_cancelled() {
                return Err(AgentError::Cancelled("Goal command superseded".into()));
            }
            let events = if matches!(
                &change,
                Change::Report {
                    completed: true,
                    ..
                }
            ) && judge == text::Judge::Evaluator
            {
                let events = self
                    .tree
                    .inputs(self.branch_id.clone(), vec![record, trigger])
                    .await?;
                self.state.lock().unwrap().last_write =
                    events.last().expect("evaluation request").sequence;
                events
            } else {
                self.append_business(vec![record, trigger], business, Some(1))?
            };
            let sequence = events[1].sequence;
            let mut lock = self.state.lock().unwrap();
            lock.recorded = lock.recorded.max(sequence);
            lock.written.insert(sequence);
            lock.last_write = lock.last_write.max(sequence);
            drop(lock);
            Ok(sequence)
        }
        .await;
        self.state.lock().unwrap().terminating = false;
        result
    }

    /// Runs `work` once under `key` while the branch is active, and records a
    /// failed write without letting it stop the branch. A request that arrives
    /// while the work runs is answered once more when it finishes, as `retry`.
    fn task<F>(self: &Arc<Self>, key: String, retry: Option<Retry>, work: F)
    where
        F: FnOnce(Arc<Self>) -> BoxFuture<'static, Result<(), AgentError>> + Send + 'static,
    {
        if self.stopped() {
            return;
        }
        let decision_task = {
            let mut state = self.state.lock().unwrap();
            if state.terminating {
                return;
            }
            if !state.in_flight.insert(key.clone()) {
                if let Some(retry) = retry {
                    state.retry.insert(key, retry);
                }
                return;
            }
            state.decision_tasks.token()
        };
        let owner = self.clone();
        let phase = self.phase.clone();
        let decisions = self.state.lock().unwrap().decisions.clone();
        self.work.spawn(async move {
            let _decision_task = decision_task;
            let active = tokio::select! {
                active = phase.activated() => active,
                _ = owner.stop.cancelled() => false,
            };
            if active
                && !owner.stopped()
                && let Err(error) = tokio::select! {
                    biased;
                    _ = decisions.cancelled() => Ok(()),
                    _ = owner.stop.cancelled() => Ok(()),
                    result = work(owner.clone()) => result,
                }
            {
                tracing::warn!(key = %key, %error, "Goal write failed");
            }
            let retry = {
                let mut state = owner.state.lock().unwrap();
                state.in_flight.remove(&key);
                state.retry.remove(&key)
            };
            // The second pass reads the fold as it stands now; C4, `decided`
            // and the conclusion's own checks deduplicate it.
            if decisions.is_cancelled() && owner.write_status.borrow().is_ok() {
                owner.resume_decisions();
            }
            match retry {
                Some(Retry::TurnEnd) => owner.decide_end(),
                Some(Retry::Settle) => owner.settle_deferred(),
                None => {}
            }
        });
    }

    /// The continuation limit, `Ok(None)` when no limit is set. A refused or
    /// unreadable document is logged and `Err(())`: the decision stops without
    /// an automatic input.
    async fn limit(&self) -> Result<Option<u64>, ()> {
        let snapshot = match self
            .settings
            .read_declared(SOURCE, SettingsScope::User, &self.stop)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "Goal settings read failed");
                return Err(());
            }
        };
        match serde_json::from_value::<Document>(snapshot.document) {
            Ok(document) => Ok(document.max_continuations),
            Err(error) => {
                tracing::warn!(%error, "Goal settings document is invalid");
                Err(())
            }
        }
    }

    async fn rebuild(self: &Arc<Self>) -> Result<u64, AgentError> {
        let through = self.timeline.committed();
        self.publication(through).await?;
        let opening = self.opening(through).await?;
        let query = |query| -> BoxFuture<'_, _> { Box::pin(self.timeline.query(query)) };
        let mut fold = crate::read::load(
            &self.branch_id,
            opening.map(|(at, _)| at),
            through,
            &query,
            &self.stop,
        )
        .await?;
        crate::read::pause_reloaded(&mut fold);
        if let Some((at, true)) = opening {
            fold.copied_live = Some(self.copied_keys(at).await?);
        }
        {
            let mut state = self.state.lock().unwrap();
            state.fold = fold;
            state.start = opening.map(|(at, _)| at);
            state.source_through = through;
        }
        self.stepped.send_replace(through);
        let previous = self.clone();
        self.work.spawn(async move {
            let active=tokio::select! {active=previous.phase.activated()=>active,_=previous.stop.cancelled()=>false};
            if !active {return;}
            let active=tokio::select! {active=previous.session.activated()=>active,_=previous.stop.cancelled()=>false};
            if !active {return;}
            for info in previous.tree.list() {
                let Some(BranchSource::Host {operation_id})=info.source else {continue;};
                if info.parent.as_deref()!=Some(previous.branch_id.as_str()) || info.create_anchor.is_none_or(|at|at>through) || fold::evaluation_of_operation(&operation_id).is_none() {continue;}
                let result=tokio::select! {result=previous.tree.end(info.id,BranchSource::Host {operation_id})=>result,_=previous.stop.cancelled()=>return};
                if let Err(error)=result && !matches!(error,AgentError::BranchEnded(_)) {previous.dispatcher.fail(error);return;}
            }
        });
        if let Some((at, _)) = opening {
            self.decide_start(at);
        }
        Ok(through)
    }

    async fn publication(&self, through: u64) -> Result<(), AgentError> {
        let mut published = self.timeline.watch();
        while published.borrow_and_update().0 < through {
            if let Some(error) = self.timeline.failure() {
                return Err(error);
            }
            published
                .changed()
                .await
                .map_err(|_| AgentError::Closed("Goal".into()))?;
        }
        Ok(())
    }

    pub(super) async fn current(&self) -> Result<GoalRead, AgentError> {
        self.await_ready().await?;
        self.await_fold().await?;
        let through = self.timeline.committed();
        let mut dispatched = self.dispatcher.watch();
        let mut write_status = self.write_status.subscribe();
        loop {
            if let Some(error) = self
                .dispatcher
                .failure()
                .or_else(|| self.timeline.failure())
            {
                return Err(error);
            }
            write_status.borrow_and_update().clone()?;
            if self.dispatcher.sequence() >= through {
                return Ok(crate::read::view(
                    &self.branch_id,
                    through,
                    &self.state.lock().unwrap().fold,
                ));
            }
            tokio::select! {
                result=dispatched.changed()=>result.map_err(|_|AgentError::Closed("Goal".into()))?,
                result=write_status.changed()=>result.map_err(|_|AgentError::Closed("Goal".into()))?,
                _=self.stop.cancelled()=>return Err(AgentError::Closed("Goal".into())),
            }
        }
    }

    fn append_business(
        &self,
        inputs: Vec<InputMessageData>,
        mut business: recovery::BusinessState,
        trigger: Option<usize>,
    ) -> Result<Vec<Arc<AgentEvent>>, AgentError> {
        let mut drafts = Vec::with_capacity(inputs.len() + 1);
        for input in inputs {
            input
                .validate()
                .map_err(|error| AgentError::InvalidInput(error.into()))?;
            drafts.push(
                EventDraft::new(EventKind::InputMessage(input))
                    .with_id(uuid::Uuid::new_v4().to_string()),
            );
        }
        if let Some(index) = trigger {
            business.input_id = drafts[index].id.clone();
        }
        drafts.push(business.draft()?);
        let events = self.timeline.append_batch_registered(drafts, |events| {
            for event in &events[..events.len() - 1] {
                let EventKind::InputMessage(data) = &event.kind else {
                    unreachable!("before preserves input kind")
                };
                data.validate()
                    .map_err(|error| AgentError::InvalidInput(error.into()))?;
            }
            let EventKind::Extension(actual) = &events.last().expect("business state").kind else {
                unreachable!("before preserves extension kind")
            };
            if actual.key() != recovery::KEY
                || recovery::BusinessState::decode(actual.data().clone())? != business
            {
                return Err(AgentError::InvalidInput(
                    "before changed Goal business state".into(),
                ));
            }
            if let EventKind::InputMessage(input) = &events[0].kind {
                let expected = Statement {
                    objective: business.objective.clone(),
                    state: business.state.clone(),
                    judge: business.judge,
                    retired: business.objective.is_none(),
                };
                let actual = input.blocks.iter().find_map(|block| {
                    let InputBlock::Raw { text, key, retire } = block;
                    (key.as_deref() == Some(text::STATE_KEY))
                        .then(|| published_statement(text, *retire))
                        .flatten()
                });
                if actual.as_ref() != Some(&expected) {
                    return Err(AgentError::InvalidInput(
                        "before changed Goal model state without its business state".into(),
                    ));
                }
            }
            if let Some(index) = trigger {
                let EventKind::InputMessage(data) = &events[index].kind else {
                    unreachable!("trigger kind")
                };
                let (attrs, operation) = fold::change_of(&data.blocks).ok_or_else(|| {
                    AgentError::InvalidInput("before removed Goal command".into())
                })?;
                let state = self.state.lock().unwrap();
                let (objective, mut status, mut judge) = state.fold.after_change(operation, &attrs);
                if objective.is_none() {
                    status = text::State::None;
                    judge = text::Judge::Model;
                }
                if (objective, status, judge)
                    != (
                        business.objective.clone(),
                        business.state.clone(),
                        business.judge,
                    )
                {
                    return Err(AgentError::InvalidInput(
                        "before changed Goal command without its state".into(),
                    ));
                }
                if business.input_id.as_deref() != Some(events[index].id.as_str()) {
                    return Err(AgentError::InvalidInput(
                        "before changed Goal trigger identity".into(),
                    ));
                }
            }
            self.state.lock().unwrap().last_write =
                events.last().expect("business result").sequence;
            Ok(())
        })?;
        Ok(events)
    }

    async fn pause_closing(&self) -> Result<(), AgentError> {
        let _write = self.write.lock().await;
        let (objective, judge, live, evaluations) = {
            let state = self.state.lock().unwrap();
            if !state.fold.running() {
                return Ok(());
            }
            (
                state.fold.objective.clone(),
                state.fold.judge,
                state.fold.live,
                state.fold.evaluations.values().cloned().collect::<Vec<_>>(),
            )
        };
        for branch in evaluations {
            if self.session.phase() >= dim_agent_runtime::SessionPhase::Closing {
                break;
            }
            match self
                .tree
                .end(
                    branch,
                    BranchSource::Host {
                        operation_id: "goal:closing".into(),
                    },
                )
                .await
            {
                Ok(()) | Err(AgentError::BranchEnded(_)) => (),
                Err(error) => return Err(error),
            }
        }
        let status = text::State::Paused("interrupted_unload".into());
        let input = text::state(
            &self.branch_id,
            objective.as_deref(),
            status.clone(),
            judge,
            text::Answered::Sequence(self.timeline.committed()),
            live,
            &self.texts,
        );
        let events = self.append_business(
            vec![input],
            recovery::BusinessState::new("pause", objective, status.clone(), judge, None),
            None,
        )?;
        self.publication(events.last().expect("paused state").sequence)
            .await?;
        self.state.lock().unwrap().fold.state = status;
        Ok(())
    }

    async fn opening(&self, through: u64) -> Result<Option<(u64, bool)>, AgentError> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(through),
                include_publishing: true,
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["branch/start".into(), "session/forked".into()],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["position.sequence".into(), "kind".into()],
                    },
                    group_by: Vec::new(),
                    pick: Some(QueryPick::Last),
                    order_by: vec![QueryOrder {
                        key: QueryOrderKey::Position,
                        direction: TimelineQueryDirection::Descending,
                    }],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("opening fields return rows")
        };
        Ok(rows.first().map(|row| {
            let [
                QueryValue::Value { value: sequence },
                QueryValue::Value { value: kind },
            ] = row.values.as_slice()
            else {
                unreachable!("opening fields exist")
            };
            (
                sequence.as_u64().expect("event sequence"),
                kind == "session/forked",
            )
        }))
    }

    async fn copied_keys(&self, opening: u64) -> Result<Live, AgentError> {
        let through = opening.saturating_sub(1);
        let mut live = Live::default();
        for (key, slot) in [
            (text::STATE_KEY, &mut live.state),
            (text::OBJECTIVE_KEY, &mut live.objective),
            (text::STATUS_KEY, &mut live.status),
        ] {
            *slot = self
                .key_at(&self.branch_id, through, key, true)
                .await?
                .unwrap_or(false);
        }
        Ok(live)
    }

    async fn key_at(
        &self,
        branch: &str,
        through: u64,
        key: &str,
        include_publishing: bool,
    ) -> Result<Option<bool>, AgentError> {
        if through == 0 {
            return Ok(None);
        }
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: Some(through),
            include_publishing,
            query: QuerySpec {
                filter: Some(live_inputs(key_block(key))),
                select: QuerySelect::Fields {
                    fields: vec!["data.blocks".into()],
                },
                group_by: vec![],
                pick: None,
                order_by: vec![QueryOrder {
                    key: QueryOrderKey::Position,
                    direction: TimelineQueryDirection::Descending,
                }],
            },
            page: Some(QueryPage {
                limit: NonZeroU64::new(INHERITED_PAGE).unwrap(),
                after: None,
            }),
        };
        loop {
            let reply = if branch == self.branch_id {
                self.timeline.query(query.clone()).await?
            } else {
                self.tree.query(branch.to_owned(), query.clone()).await?
            };
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("key fields return rows")
            };
            for row in rows {
                let [QueryValue::Value { value: blocks }] = row.values.as_slice() else {
                    unreachable!("input blocks exist")
                };
                let blocks: Vec<InputBlock> = serde_json::from_value(blocks.clone())
                    .expect("input blocks follow their event contract");
                for block in blocks.iter().rev() {
                    let InputBlock::Raw {
                        text: raw,
                        key: block_key,
                        retire,
                    } = block;
                    if block_key.as_deref() == Some(key) && text::marker(raw).is_some() {
                        return Ok(Some(!retire));
                    }
                }
            }
            query.page.as_mut().unwrap().after = reply.next;
            if query.page.as_ref().unwrap().after.is_none() {
                return Ok(None);
            }
        }
    }

    /// Folds one consumed local event and takes the decisions it carries. The
    /// rebuild's facts take decisions only through its closing steps.
    fn ingest(self: &Arc<Self>, event: &AgentEvent, live: bool) -> Result<(), AgentError> {
        let sequence = event.sequence;
        if let EventKind::Extension(data) = &event.kind
            && data.key() == recovery::KEY
        {
            let business = recovery::BusinessState::decode(data.data().clone())?;
            let mut state = self.state.lock().unwrap();
            business.apply(&mut state.fold);
            state.fold.state_after = sequence;
            state.fold.record_after = sequence;
            state.source_through = sequence;
            self.stepped.send_replace(sequence);
            return Ok(());
        }
        let turn_start = {
            let mut state = self.state.lock().unwrap();
            let turn_start = state.fold.turn_start;
            state.fold.apply(event);
            if fold::is_opening(&event.kind) {
                state.start = Some(sequence);
            }
            let start = state.start;
            state.fold.compact(start);
            state.source_through = sequence;

            let recorded = state.recorded;
            let State { fold, written, .. } = &mut *state;
            written.retain(|after| *after == recorded || fold.needs_decision(*after, start));
            self.stepped.send_replace(sequence);
            turn_start
        };
        if fold::is_opening(&event.kind) {
            self.state.lock().unwrap().start = Some(sequence);
            if live {
                self.decide_start(sequence);
            }
            return Ok(());
        }
        if !live {
            return Ok(());
        }
        match &event.kind {
            EventKind::BranchCreateFailed(data) => {
                let branch = match data {
                    dim_agent_types::BranchCreateFailedData::Failed { branch, .. } => branch,
                    dim_agent_types::BranchCreateFailedData::Cancelled { branch, .. } => branch,
                };
                let evaluation = self.state.lock().unwrap().fold.evaluation_of(branch);
                if let Some(evaluation) = evaluation {
                    self.evaluation_failed(evaluation, sequence);
                }
            }
            EventKind::BranchCreate(_) => self.settle(),
            EventKind::BranchEnded(data) => {
                let evaluation = self.state.lock().unwrap().fold.evaluation_of(&data.branch);
                if let Some(evaluation) = evaluation {
                    let state = self.state.lock().unwrap();
                    if state.fold.is_current(evaluation)
                        && !state.fold.conclusions.contains_key(&evaluation)
                    {
                        drop(state);
                        self.evaluation_failed(evaluation, sequence);
                    }
                }
            }
            EventKind::InputDeleted(_) => self.settle_deferred(),
            EventKind::TurnEnd(data) => self.turn_decision(sequence, turn_start, data.clone()),
            EventKind::StopInterrupt(data) if data.scope == InterruptScope::Turn => {
                self.interrupted(sequence)
            }
            EventKind::InputMessage(data) => {
                if let Some(conclusion) = data.blocks.first().and_then(|block| match block {
                    InputBlock::Raw { text, .. } => fold::conclusion_of(text),
                }) {
                    self.conclude(sequence, conclusion);
                }
                self.answer_change();
                self.settle();
            }
            // A cancelled background end does not wake (BackgroundExec's
            // `wakes`); when it closes the last open task of an idle branch,
            // the deferred conclusion or turn end decides now. Other statuses
            // are followed by their wake input, whose turn end decides.
            EventKind::ExecEnded(data) if data.status == ExecStatus::Cancelled => {
                self.settle_deferred()
            }
            _ => {}
        }
        Ok(())
    }

    /// Takes what a cancelled background end released, once the branch is idle
    /// and no task is open (A.4, Pursuit §5): a conclusion the wait deferred,
    /// or the turn-end decision. The cancelled status is read from the fact,
    /// so this cannot race a `completed` end's wake.
    fn settle_deferred(self: &Arc<Self>) {
        let pending = {
            let state = self.state.lock().unwrap();
            let fold = &state.fold;
            if !(fold.running()
                && !fold.open_tasks()
                && !fold.turn_open()
                && !fold.pending_driving())
            {
                return;
            }
            fold.pending_conclusion()
                .map(|(sequence, conclusion)| (sequence, conclusion.clone()))
        };
        match pending {
            Some((sequence, conclusion)) => self.conclude(sequence, conclusion),
            None => self.decide_end(),
        }
    }

    /// Clears a goal inherited from another branch, once, on a branch that
    /// has no goal history of its own (§3). The inherited read is owned work:
    /// it waits for activation, runs once per `at` and not after a stop. A
    /// failed read writes nothing and is logged with its key, and the next
    /// load decides the opening again.
    fn decide_start(self: &Arc<Self>, at: u64) {
        let (decided, local_blocks, copied) = {
            let state = self.state.lock().unwrap();
            (
                state.fold.is_decided(at),
                state.fold.local_blocks,
                state.fold.copied_live,
            )
        };
        if decided || local_blocks > 0 {
            return;
        }
        if let Some(live) = copied {
            if live.any() {
                self.started_clear(at, live);
            }
            return;
        }
        self.task(format!("start:{at}"), None, move |owner| {
            Box::pin(async move {
                let live = owner.inherited_keys().await?;
                if live.any() {
                    owner.started_clear(at, live);
                }
                Ok(())
            })
        });
    }

    fn started_clear(self: &Arc<Self>, at: u64, live: Live) {
        self.task(format!("clear:{at}"), None, move |owner| {
            Box::pin(async move {
                let _write = owner.write_guard().await?;
                let write = {
                    let state = owner.state.lock().unwrap();
                    !state.fold.is_decided(at)
                        && state.fold.local_blocks == 0
                        && state.fold.last_change == 0
                };
                if !write {
                    return Ok(());
                }
                owner
                    .write(text::start_clear(&owner.branch_id, at, live))
                    .await?;
                let mut state = owner.state.lock().unwrap();
                state.recorded = state.recorded.max(at);
                Ok(())
            })
        });
    }

    fn interrupted(self: &Arc<Self>, at: u64) {
        self.task(format!("interrupt:{at}"), None, move |owner| {
            Box::pin(async move {
                let reason = {
                    let state = owner.state.lock().unwrap();
                    match fold::decide_interrupt(&state.fold, at) {
                        Some(Decision::Pause { reason }) => reason,
                        _ => return Ok(()),
                    }
                };
                owner
                    .record(at, Some(text::State::Paused(reason.to_owned())), false)
                    .await
            })
        });
    }

    async fn record(
        self: &Arc<Self>,
        after: u64,
        status: Option<text::State>,
        answer: bool,
    ) -> Result<(), AgentError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let owner = self.clone();
        self.tasks.spawn(async move {
            let result = owner.record_inner(after, status, answer).await;
            owner.state.lock().unwrap().terminating = false;
            if let Err(Err(error)) = reply.send(result) {
                tracing::warn!(%error, "Goal terminal state could not be saved");
                let _ = owner.write_status.send_replace(Err(error));
            }
        });
        result
            .await
            .map_err(|_| AgentError::Closed("Goal".into()))?
    }

    async fn record_inner(
        self: &Arc<Self>,
        after: u64,
        status: Option<text::State>,
        answer: bool,
    ) -> Result<(), AgentError> {
        let _write = self.write_guard().await?;
        {
            let state = self.state.lock().unwrap();
            if answer && state.fold.pending_change() != Some(after)
                || state.fold.last_change > after
                || after < state.recorded
                || state.written.contains(&after)
            {
                return Ok(());
            }
        }
        self.settle_decisions().await?;
        self.write_status.borrow().clone()?;
        let (objective, status, judge, live) = {
            let state = self.state.lock().unwrap();
            if state.fold.last_change > after || after < state.recorded {
                return Ok(());
            }
            (
                state.fold.objective.clone(),
                status.unwrap_or_else(|| state.fold.state.clone()),
                state.fold.judge,
                state.fold.live,
            )
        };
        let operation = match &status {
            text::State::Active => "resume",
            text::State::Paused(_) => "pause",
            text::State::Completed(_) => "completed",
            text::State::Blocked(_) => "blocked",
            text::State::None => "clear",
        };
        let input = text::state(
            &self.branch_id,
            objective.as_deref(),
            status.clone(),
            judge,
            text::Answered::Sequence(after),
            live,
            &self.texts,
        );
        let business = recovery::BusinessState::new(operation, objective, status, judge, None);
        self.append_business(vec![input], business, None)?;
        let mut state = self.state.lock().unwrap();
        state.written.insert(after);
        state.recorded = state.recorded.max(after);
        Ok(())
    }

    async fn settle_decisions(&self) -> Result<(), AgentError> {
        let previous = {
            let mut state = self.state.lock().unwrap();
            state.terminating = true;
            state.decisions.cancel();
            state.decisions = CancellationToken::new();
            std::mem::take(&mut state.decision_tasks)
        };
        previous.close();
        previous.wait().await;
        self.await_fold().await?;
        let evaluations = {
            let state = self.state.lock().unwrap();
            state
                .fold
                .evaluations
                .iter()
                .filter(|(_, branch)| {
                    !state.fold.ended.contains(*branch) && !state.fold.failed.contains(*branch)
                })
                .map(|(at, branch)| (*at, branch.clone()))
                .collect::<Vec<_>>()
        };
        for (at, branch) in evaluations {
            match self
                .tree
                .end(
                    branch,
                    BranchSource::Host {
                        operation_id: fold::evaluation_operation(at),
                    },
                )
                .await
            {
                Ok(()) | Err(AgentError::BranchEnded(_)) => (),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// States the newest change trigger, once (D3): the record names the
    /// trigger in `after`, so the fold and every client read the same state.
    fn answer_change(self: &Arc<Self>) {
        let Some(last) = self.state.lock().unwrap().fold.pending_change() else {
            return;
        };
        self.task(format!("state:{last}"), None, move |owner| {
            Box::pin(async move { owner.record(last, None, true).await })
        });
    }

    /// Whether this branch already committed its `branch/create` for `after`:
    /// from then on the creation records its own outcome, so a failed wait
    /// only means the caller stopped waiting (Pursuit §4.1).
    async fn created(&self, after: u64) -> Result<bool, AgentError> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(dim_agent_types::TimelinePosition {
                    branch_id: self.branch_id.clone(),
                    sequence: after,
                }),
                through: Some(self.timeline.committed()),
                include_publishing: true,
                query: QuerySpec {
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
                                    path: "data.source.operation_id".into(),
                                },
                                values: vec![fold::evaluation_operation(after).into()],
                            },
                        ],
                    }),
                    select: QuerySelect::Exists,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: None,
            })
            .await?;
        let QueryOutput::Exists { exists } = reply.result else {
            unreachable!("existence selection returns a boolean")
        };
        Ok(exists)
    }

    /// Whether the turn `turn_id` made a tool call: one bounded query from the
    /// turn's start, limit one (A.4 C7). The kind is not folded (A.6).
    async fn turn_had_tool_call(&self, turn_start: u64, turn_id: &str) -> Result<bool, AgentError> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(dim_agent_types::TimelinePosition {
                    branch_id: self.branch_id.clone(),
                    sequence: turn_start,
                }),
                through: Some(self.timeline.committed()),
                include_publishing: true,
                query: QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["step/tool-call".into()],
                            },
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.turn_id".into(),
                                },
                                values: vec![turn_id.into()],
                            },
                        ],
                    }),
                    select: QuerySelect::Exists,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: None,
            })
            .await?;
        let QueryOutput::Exists { exists } = reply.result else {
            unreachable!("existence selection returns a boolean")
        };
        Ok(exists)
    }

    /// The turn-end decision (A.4, Pursuit §4.1): an evaluator creation when
    /// the judge asks for one, otherwise the continuation limit.
    async fn do_turn_end(self: &Arc<Self>, after: u64) -> Result<(), AgentError> {
        let (request, evaluation, objective) = {
            let state = self.state.lock().unwrap();
            let fold = &state.fold;
            if fold.turn_end.as_ref().map(|end| end.sequence) != Some(after) {
                return Ok(());
            }
            (
                fold.request_in_turn(after)
                    .map(|request| request.summary.clone()),
                fold::decide_evaluation(fold, after),
                fold.objective.clone().unwrap_or_default(),
            )
        };
        if let Some(summary) = request
            && evaluation
        {
            let cancel = self.stop.clone();
            let cancelled: Arc<
                dyn Fn() -> BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync,
            > = Arc::new(move || {
                let cancel = cancel.clone();
                Box::pin(async move {
                    cancel.cancelled().await;
                    Ok(())
                })
            });
            let options = BranchOptions {
                prompt: text::evaluator_prompt(
                    &self.branch_id,
                    &after.to_string(),
                    &objective,
                    &summary,
                ),
                source: BranchSource::Host {
                    operation_id: fold::evaluation_operation(after),
                },
            };
            let created = self
                .tree
                .create(self.branch_id.clone(), after, options, cancelled)
                .await;
            if let Err(error) = &created {
                tracing::warn!(branch_id = %self.branch_id, evaluation = after, %error, "Goal evaluator creation failed");
            }
            if created.is_err()
                && !self.stopped()
                && !self.created(after).await?
                && self.state.lock().unwrap().fold.is_current(after)
            {
                self.record(
                    after,
                    Some(text::State::Paused("evaluation_failed".to_owned())),
                    false,
                )
                .await?;
            }
            self.settle();
            return Ok(());
        }
        let Ok(limit) = self.limit().await else {
            return Ok(());
        };
        if self.stopped() {
            return Ok(());
        }
        // C7's answer comes from the ending turn, read at decision time and
        // only when a continuation drove it; `step/tool-call` is not a fold
        // kind (A.6, #1113). An open background task defers the decision, so
        // it needs no answer now.
        let progress = {
            let (end, open) = {
                let state = self.state.lock().unwrap();
                if state.fold.turn_end.as_ref().map(|end| end.sequence) != Some(after)
                    || state.fold.interrupt_after(after).is_some()
                {
                    return Ok(());
                }
                (state.fold.turn_end.clone(), state.fold.open_tasks())
            };
            let Some(end) = end else {
                return Ok(());
            };
            if open || !end.continuation_driven {
                None
            } else if self
                .turn_had_tool_call(end.turn_start, &end.turn_id)
                .await?
            {
                Some(fold::Progress::ToolCall)
            } else {
                Some(fold::Progress::NoToolCall)
            }
        };
        if self.stopped() {
            return Ok(());
        }
        let write = self.write_guard().await?;
        let decision = {
            let state = self.state.lock().unwrap();
            if state.fold.turn_end.as_ref().map(|end| end.sequence) != Some(after)
                || state.fold.interrupt_after(after).is_some()
            {
                return Ok(());
            }
            fold::decide_turn_end(&state.fold, limit, progress)
        };
        let Some(decision) = decision else {
            return Ok(());
        };
        match decision {
            Decision::Continuation { count, idle } => {
                let input =
                    text::continuation(&self.branch_id, count, limit, idle, after, &self.texts);
                self.write(input).await?;
            }
            Decision::Pause { reason } => {
                drop(write);
                self.record(after, Some(text::State::Paused(reason.to_owned())), false)
                    .await?;
            }
        }
        Ok(())
    }

    /// The latest `turn/end` without a live decision: an open decision the
    /// rebuild still owes after an input was deleted (§5).
    fn decide_end(self: &Arc<Self>) {
        let end = self.state.lock().unwrap().fold.turn_end.clone();
        let Some(TurnEnd { sequence, .. }) = end else {
            return;
        };
        let interrupt = self.state.lock().unwrap().fold.interrupt_after(sequence);
        if let Some(interrupt) = interrupt {
            self.interrupted(interrupt);
            return;
        }
        let decide = {
            let state = self.state.lock().unwrap();
            state.fold.running() && !state.fold.is_decided(sequence)
        };
        if !decide {
            return;
        }
        self.task(
            format!("turn:{sequence}"),
            Some(Retry::TurnEnd),
            move |owner| {
                Box::pin(async move {
                    owner.resolved().await;
                    let skip = {
                        let state = owner.state.lock().unwrap();
                        state.role.is_some()
                            || state.fold.turn_end.as_ref().map(|end| end.sequence)
                                != Some(sequence)
                    };
                    if owner.stopped() || skip {
                        return Ok(());
                    }
                    owner.do_turn_end(sequence).await
                })
            },
        );
    }

    /// The decision a live `turn/end` at `sequence` takes: the evaluator
    /// role's report, or the branch's own turn-end decision.
    fn turn_decision(self: &Arc<Self>, sequence: u64, turn_start: u64, end: TurnEndData) {
        self.task(
            format!("turn:{sequence}"),
            Some(Retry::TurnEnd),
            move |owner| {
                Box::pin(async move {
                    owner.resolved().await;
                    if owner.stopped() {
                        return Ok(());
                    }
                    let (role, decide) = {
                        let state = owner.state.lock().unwrap();
                        (
                            state.role.clone(),
                            state.fold.running() && !state.fold.is_decided(sequence),
                        )
                    };
                    if role.is_some() {
                        return owner.evaluated(sequence, turn_start, &end).await;
                    }
                    if decide {
                        owner.do_turn_end(sequence).await
                    } else {
                        Ok(())
                    }
                })
            },
        );
    }

    /// The evaluator line's report for the turn that ended at `sequence`
    /// (Pursuit §4.2, §5).
    async fn evaluated(
        self: &Arc<Self>,
        sequence: u64,
        turn_start: u64,
        end: &TurnEndData,
    ) -> Result<(), AgentError> {
        let _write = self.write_guard().await?;
        let role = self.state.lock().unwrap().role.clone();
        let Some(role) = role else {
            return Ok(());
        };
        if self
            .state
            .lock()
            .unwrap()
            .fold
            .turn_end
            .as_ref()
            .map(|end| end.sequence)
            != Some(sequence)
        {
            return Ok(());
        }
        if !matches!(end.outcome, TurnOutcome::Completed { .. }) {
            return self
                .report(&role, "failed", "the evaluator's turn did not complete")
                .await;
        }
        let text = self.reply(turn_start, sequence, &end.turn_id).await?;
        if let Some((result, body)) = fold::reply_conclusion(&text) {
            return self.report(&role, result_name(result), &body).await;
        }
        if self.state.lock().unwrap().fold.turn_ends == 1 {
            self.write(text::evaluate_again(&self.branch_id, sequence))
                .await?;
            return Ok(());
        }
        self.report(&role, "continue", &fold::truncate(&text)).await
    }

    /// Writes one conclusion about this evaluator's evaluation to the branch
    /// it reports to.
    async fn report(&self, role: &Role, result: &str, text: &str) -> Result<(), AgentError> {
        self.tree
            .input(
                role.branch.clone(),
                text::conclusion(
                    &role.branch,
                    &self.branch_id,
                    &role.evaluation.to_string(),
                    result,
                    text,
                ),
            )
            .await?;
        Ok(())
    }

    /// The reply of the turn that ended at `through`: the text deltas of the
    /// request its last `step/model-start` names (Pursuit §4.2).
    async fn reply(
        &self,
        turn_start: u64,
        through: u64,
        turn_id: &str,
    ) -> Result<String, AgentError> {
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: Some(dim_agent_types::TimelinePosition {
                branch_id: self.branch_id.clone(),
                sequence: turn_start,
            }),
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: vec!["step/model-start".into()],
                        },
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "data.turn_id".into(),
                            },
                            values: vec![turn_id.into()],
                        },
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data.request_id".into()],
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
        let reply = self.timeline.query(query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("field selection returns rows")
        };
        let Some(row) = rows.first() else {
            return Ok(String::new());
        };
        let [QueryValue::Value { value: request }] = row.values.as_slice() else {
            unreachable!("model start has a request id")
        };
        let Some(QueryFilter::And { conditions }) = &mut query.query.filter else {
            unreachable!()
        };
        conditions[0] = QueryFilter::In {
            value: QueryOperand::Field {
                path: "kind".into(),
            },
            values: vec!["step/text".into()],
        };
        conditions.push(QueryFilter::In {
            value: QueryOperand::Field {
                path: "data.request_id".into(),
            },
            values: vec![request.clone()],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec!["data.delta".into()],
        };
        query.query.pick = None;
        query.page.as_mut().unwrap().limit = NonZeroU64::new(REPLY_PAGE).unwrap();
        let mut text = String::new();
        loop {
            let reply = self.timeline.query(query.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("field selection returns rows")
            };
            for row in rows {
                let [
                    QueryValue::Value {
                        value: serde_json::Value::String(delta),
                    },
                ] = row.values.as_slice()
                else {
                    unreachable!("text event has a delta")
                };
                text.push_str(delta);
            }
            query.page.as_mut().unwrap().after = reply.next;
            if query.page.as_ref().unwrap().after.is_none() {
                return Ok(text);
            }
        }
    }

    fn evaluation_failed(self: &Arc<Self>, evaluation: u64, at: u64) {
        self.task(format!("failed:{evaluation}"), None, move |owner| {
            Box::pin(async move {
                let current = owner.state.lock().unwrap().fold.is_current(evaluation);
                if !current {
                    return Ok(());
                }
                owner
                    .record(
                        at,
                        Some(text::State::Paused("evaluation_failed".to_owned())),
                        false,
                    )
                    .await
            })
        });
    }

    /// The conclusion the evaluator wrote decides the evaluation it names
    /// (Pursuit §4.1). A conclusion the background wait deferred is taken
    /// again through here when the branch settles (Pursuit §5).
    fn conclude(self: &Arc<Self>, sequence: u64, conclusion: Conclusion) {
        self.task(
            format!("conclusion:{sequence}"),
            Some(Retry::Settle),
            move |owner| Box::pin(async move { owner.run_conclusion(sequence, conclusion).await }),
        );
    }

    async fn run_conclusion(
        self: &Arc<Self>,
        sequence: u64,
        conclusion: Conclusion,
    ) -> Result<(), AgentError> {
        let evaluation = conclusion.evaluation;
        let action = {
            let state = self.state.lock().unwrap();
            fold::decide_conclusion(&state.fold, sequence, &conclusion)
        };
        let Some(action) = action else {
            return Ok(());
        };
        let named = evaluation.to_string();
        match action {
            ConclusionAction::Failed => {
                self.record(
                    sequence,
                    Some(text::State::Paused("evaluation_failed".to_owned())),
                    false,
                )
                .await?;
            }
            ConclusionAction::Completed(text) => {
                self.record(sequence, Some(text::State::Completed(text)), false)
                    .await?;
            }
            ConclusionAction::Blocked(text) => {
                self.record(sequence, Some(text::State::Blocked(text)), false)
                    .await?;
            }
            ConclusionAction::Continue => {
                let Ok(limit) = self.limit().await else {
                    return Ok(());
                };
                if self.stopped() {
                    return Ok(());
                }
                let end = {
                    let state = self.state.lock().unwrap();
                    if fold::decide_conclusion(&state.fold, sequence, &conclusion)
                        != Some(ConclusionAction::Continue)
                    {
                        return Ok(());
                    }
                    state.fold.turn_end.clone()
                };
                let Some(end) = end else {
                    return Ok(());
                };
                // C7 measures the branch's own continuations; the evaluator
                // judged this turn, so this path reads no progress (Pursuit §5).
                let write = self.write_guard().await?;
                let decision = {
                    let state = self.state.lock().unwrap();
                    if state.written.contains(&sequence)
                        || fold::decide_conclusion(&state.fold, sequence, &conclusion)
                            != Some(ConclusionAction::Continue)
                        || state.fold.turn_end.as_ref().map(|end| end.sequence)
                            != Some(end.sequence)
                    {
                        return Ok(());
                    }
                    fold::decide_evaluated_continuation(&state.fold, limit)
                };
                let Some(decision) = decision else {
                    return Ok(());
                };
                match decision {
                    Decision::Continuation { count, idle } => {
                        let input = text::evaluated_continuation(
                            &self.branch_id,
                            count,
                            limit,
                            idle,
                            evaluation,
                            &named,
                        );
                        // The input is committed before the fold consumes it; a
                        // retry in between must not answer it again.
                        if self.state.lock().unwrap().written.contains(&sequence) {
                            return Ok(());
                        }
                        self.write(input).await?;
                        self.state.lock().unwrap().written.insert(sequence);
                    }
                    Decision::Pause { reason } => {
                        drop(write);
                        self.record(
                            sequence,
                            Some(text::State::Paused(reason.to_owned())),
                            false,
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Ends the evaluator lines no evaluation needs any more (Pursuit §13 E2).
    fn settle(self: &Arc<Self>) {
        let evaluations: Vec<(u64, String)> = {
            let state = self.state.lock().unwrap();
            state
                .fold
                .evaluations
                .iter()
                .map(|(evaluation, branch)| (*evaluation, branch.clone()))
                .collect()
        };
        for (evaluation, evaluator) in evaluations {
            let settled = {
                let state = self.state.lock().unwrap();
                state.fold.ended.contains(&evaluator)
                    || state.fold.failed.contains(&evaluator)
                    || !(state.fold.conclusions.contains_key(&evaluation)
                        || !state.fold.is_current(evaluation))
            };
            if settled {
                continue;
            }
            self.task(format!("settle:{evaluation}"), None, move |owner| {
                Box::pin(async move {
                    owner.resolved().await;
                    if !owner.session.activated().await {
                        return Ok(());
                    }
                    let skip = {
                        let state = owner.state.lock().unwrap();
                        state.fold.ended.contains(&evaluator)
                            || !(state.fold.conclusions.contains_key(&evaluation)
                                || !state.fold.is_current(evaluation))
                    };
                    if owner.stopped() || skip {
                        return Ok(());
                    }
                    match owner
                        .tree
                        .end(
                            evaluator.clone(),
                            BranchSource::Host {
                                operation_id: fold::evaluation_operation(evaluation),
                            },
                        )
                        .await
                    {
                        Ok(()) | Err(AgentError::BranchEnded(_)) => Ok(()),
                        Err(error) => Err(error),
                    }
                })
            });
        }
    }

    /// The Goal keys an inherited prefix left live, read newest first one
    /// ancestor at a time, if the branch has no copied facts to answer it
    /// (A11, D5).
    ///
    /// `Timeline::ancestors` is root first and the direct parent is pushed
    /// after that list (`timeline_tree.rs`, `restore` and `branch`), so the
    /// reverse is nearest first. Each key is decided by its newest block: the
    /// start clear retires all three, and an old key a history left live is
    /// retired with it.
    async fn inherited_keys(&self) -> Result<Live, AgentError> {
        let mut live = Live::default();
        for (key, slot) in [
            (text::STATE_KEY, &mut live.state),
            (text::OBJECTIVE_KEY, &mut live.objective),
            (text::STATUS_KEY, &mut live.status),
        ] {
            for (branch, cut) in self.timeline.ancestors().iter().rev() {
                if let Some(value) = self.key_at(branch, *cut, key, false).await? {
                    *slot = value;
                    break;
                }
            }
        }
        Ok(live)
    }
}

/// What a `goal:state` block states, in the fields the record writer composes
/// from the fold (R6.1).
#[derive(Clone, Debug, PartialEq)]
struct Statement {
    objective: Option<String>,
    state: text::State,
    judge: text::Judge,
    retired: bool,
}

/// What the published block `raw` states, by the readers the record writer's
/// counterpart uses. A block this version cannot read — an older format with
/// no `<objective>` element — is no statement at all, which never matches the
/// fold.
fn published_statement(raw: &str, retire: bool) -> Option<Statement> {
    let attrs = text::marker(raw)?;
    if attrs.get("kind").map(String::as_str) != Some("state") {
        return None;
    }
    if retire {
        return Some(Statement {
            objective: None,
            state: text::State::None,
            judge: text::Judge::Model,
            retired: true,
        });
    }
    let (objective, summary) = text::state_parts(raw);
    let objective = objective.filter(|objective| !objective.is_empty())?;
    let state = match attrs.get("status").map(String::as_str)? {
        "active" => text::State::Active,
        "paused" => text::State::Paused(
            attrs
                .get("reason")
                .filter(|reason| !reason.is_empty())
                .cloned()
                .unwrap_or_else(|| "user".to_owned()),
        ),
        "completed" => text::State::Completed(summary.unwrap_or_default()),
        "blocked" => text::State::Blocked(summary.unwrap_or_default()),
        "none" => text::State::None,
        _ => return None,
    };
    Some(Statement {
        objective: Some(objective),
        state,
        judge: text::Judge::of(&attrs),
        retired: false,
    })
}

fn result_name(result: fold::ConclusionKind) -> &'static str {
    match result {
        fold::ConclusionKind::Done => "done",
        fold::ConclusionKind::Continue => "continue",
        fold::ConclusionKind::Blocked => "blocked",
        fold::ConclusionKind::Failed => "failed",
    }
}

pub(crate) fn key_block(key: &str) -> QueryFilter {
    QueryFilter::And {
        conditions: vec![
            QueryFilter::Any {
                field: "data.blocks".into(),
                condition: Box::new(QueryFilter::In {
                    value: QueryOperand::Element { path: "key".into() },
                    values: vec![key.into()],
                }),
            },
            QueryFilter::TextCandidate {
                field: "data".into(),
                partition_by: vec![],
                terms: vec![key.into()],
            },
        ],
    }
}

pub(crate) fn live_inputs(condition: QueryFilter) -> QueryFilter {
    QueryFilter::And {
        conditions: vec![
            QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: vec!["input/message".into()],
            },
            condition,
            QueryFilter::Not {
                condition: Box::new(QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["input/deleted".into()],
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
    }
}
