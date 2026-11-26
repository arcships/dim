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
use dim_app_server::settings::{Settings, SettingsScope};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) mod recovery;
mod restore;

const SOURCE: &str = "goal";

/// How long the closing pause waits for the fold to step the facts already
/// committed before it composes the branch's last record. A branch that
/// stopped folding writes none, so the close never waits on one.
const CLOSING_GRACE: Duration = Duration::from_millis(250);

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
    settings: Key<Settings>,
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
                #[cfg(feature = "testing")]
                session_id: identity.session_id.clone(),
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
                        if let Err(error) = closing.pause_closing().await {
                            closing.dispatcher.fail(error);
                        }
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

/// What a settlement's task admission did with the work a caller asked for
/// (#2158).
#[derive(PartialEq, Eq)]
enum Admission {
    /// The work runs in this generation, now or through its retry.
    Taken,
    /// A settlement holds admission: the caller keeps the decision it was
    /// about to state, for that settlement to state before its own record.
    Held,
}

/// The fact a state record answers, and the check that fact still makes at the
/// fold the record is composed from (D3, C4, C5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answers {
    /// The newest change trigger, which `after` names.
    Change,
    /// The pause the turn end at `after` decides, with the limit and progress
    /// its decision read: the record states the reason the fold gives at the
    /// writer's own cutoff.
    TurnEnd {
        limit: Option<u64>,
        progress: Option<fold::Progress>,
    },
    /// The pause the `continue` conclusion at `after` of `evaluation` decides.
    EvaluatedContinuation { evaluation: u64, limit: Option<u64> },
    /// The pause the failed conclusion at `after` of `evaluation` states.
    Conclusion(u64),
    /// The pause the failed evaluation states: its creation failed or its
    /// line ended before a conclusion.
    EvaluationFailed(u64),
    /// The fact `after` names, decided once by its caller.
    Fact,
}

impl Answers {
    /// The pause `after` still decides at `fold`, with the reason that fold
    /// gives it now.
    fn pause(&self, fold: &fold::Fold, after: u64) -> Option<&'static str> {
        match *self {
            Answers::TurnEnd { limit, progress } => fold.pause_at_end(after, limit, progress),
            Answers::EvaluatedContinuation { evaluation, limit } => {
                fold.pause_at_conclusion(after, evaluation, limit)
            }
            _ => None,
        }
    }
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
    /// The closing pause already gave its delivery the grace.
    closing_grace: bool,
    /// The state records a settlement refused to admit while it held task
    /// admission: their facts committed before the record of the change that
    /// follows, so it states them first (#2158). One a record leaves undecided
    /// stays for a later change, which states it or refuses it as superseded;
    /// one whose append failed keeps its place for that later change too.
    deferred: VecDeque<DeferredRecord>,
}

/// A state record a settlement refused to admit, with the fact it answers.
struct DeferredRecord {
    after: u64,
    status: text::State,
    answers: Answers,
}

impl State {
    /// Whether a newer state statement already answered `after`: a change that
    /// folded after it, a newer record, or this record's own earlier pass.
    fn answered(&self, after: u64) -> bool {
        self.fold.last_change > after || after < self.recorded || self.written.contains(&after)
    }

    /// Whether the record answering `after` with `answers` must not be written:
    /// the fold has moved past it.
    fn stale(&self, after: u64, answers: Answers) -> bool {
        if self.answered(after) {
            return true;
        }
        match answers {
            Answers::Change => self.fold.pending_change() != Some(after),
            Answers::TurnEnd { .. } | Answers::EvaluatedContinuation { .. } => {
                answers.pause(&self.fold, after).is_none()
            }
            Answers::Conclusion(evaluation) => !self.fold.conclusion_decides(after, evaluation),
            Answers::EvaluationFailed(evaluation) => !self.fold.pauses_at_failure(evaluation),
            Answers::Fact => false,
        }
    }
}

struct Owner {
    branch_id: String,
    #[cfg(feature = "testing")]
    session_id: String,
    timeline: Arc<Timeline>,
    dispatcher: Arc<Dispatcher>,
    tree: Arc<TreeCommands>,
    settings: Arc<Settings>,
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

    /// [`Self::write`] only while the branch's last assigned sequence is still
    /// `head`. `None` means a fact committed behind `head`: nothing was
    /// written and the caller decides again.
    async fn write_if_head(
        &self,
        input: InputMessageData,
        head: u64,
    ) -> Result<Option<Arc<AgentEvent>>, AgentError> {
        let event = self
            .tree
            .input_if_head(self.branch_id.clone(), input, head)
            .await?;
        if let Some(event) = &event {
            let mut state = self.state.lock().unwrap();
            state.last_write = state.last_write.max(event.sequence);
        }
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
        let result = async {
            self.settle_decisions().await?;
            #[cfg(feature = "testing")]
            crate::testing::stage_held(
                &self.session_id,
                &self.branch_id,
                crate::testing::Stage::Change,
                self.timeline.committed(),
            )
            .await;
            // A Set or Resume loads the branch's current work before it
            // writes: that wait holds task admission while it waits for
            // publication and queries the Timeline, so a decision that folds
            // there is refused admission after the settlement's own drain. The
            // record below answers a trigger newer than their facts, so the
            // change states them first, and it writes only while the head its
            // last drain reached still stands (#2158).
            let events = loop {
                if matches!(change, Change::Set { .. } | Change::Resume) {
                    self.load_current_work().await?;
                }
                let head = self.write_deferred().await?;
                let trigger = change.trigger(&self.branch_id);
                let (objective, state, judge, live, decisions) = {
                    let lock = self.state.lock().unwrap();
                    if self.timeline.committed() != head {
                        continue;
                    }
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
                let record = text::state(
                    &self.branch_id,
                    objective.as_deref(),
                    state.clone(),
                    judge,
                    text::Answered::Trigger(&trigger_id),
                    live,
                    &self.texts,
                );
                if decisions.is_cancelled() {
                    return Err(AgentError::Cancelled("Goal command superseded".into()));
                }
                #[cfg(feature = "testing")]
                crate::testing::stage_held(
                    &self.session_id,
                    &self.branch_id,
                    crate::testing::Stage::Batch,
                    head,
                )
                .await;
                // The evaluation request states no change of its own (A16):
                // its record keeps the running goal, so the batch carries no
                // business event.
                let business = if matches!(
                    &change,
                    Change::Report {
                        completed: true,
                        ..
                    }
                ) && judge == text::Judge::Evaluator
                {
                    None
                } else {
                    Some(recovery::BusinessState::new(
                        operation, objective, state, judge, None,
                    ))
                };
                let Some(events) =
                    self.append_business_if_head(vec![record, trigger], business, Some(1), head)?
                else {
                    continue;
                };
                break events;
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
        match &result {
            Ok(_) => self.resume_settlement(),
            // A change that failed states no state of its own, so the
            // interrupt the settlement deferred waits for a later settlement
            // that succeeds instead of reaching an append of its own; the
            // decisions that settlement cancelled decide nothing after this
            // release, because the settlement waited for every one of them
            // before it wrote (Pursuit §4.1, §13 E2, #2119).
            Err(_) => {
                self.release_settlement();
            }
        }
        result
    }

    /// Runs `work` once under `key` while the branch is active, and records a
    /// failed write without letting it stop the branch. A request that arrives
    /// while the work runs is answered once more when it finishes, as `retry`.
    /// A settlement that holds admission takes no work: the caller keeps the
    /// decision and states it before that settlement's own record (#2158).
    fn task<F>(self: &Arc<Self>, key: String, retry: Option<Retry>, work: F) -> Admission
    where
        F: FnOnce(Arc<Self>) -> BoxFuture<'static, Result<(), AgentError>> + Send + 'static,
    {
        if self.stopped() {
            return Admission::Taken;
        }
        // The tracker token and the cancellation token are one generation: a
        // settlement that swaps them between two reads would admit this task
        // with the old tracker and a token no settlement cancels, leaving
        // `settle_decisions` waiting for work it cannot stop.
        let (decision_task, decisions) = {
            let mut state = self.state.lock().unwrap();
            if state.terminating {
                return Admission::Held;
            }
            if !state.in_flight.insert(key.clone()) {
                if let Some(retry) = retry {
                    state.retry.insert(key, retry);
                }
                return Admission::Taken;
            }
            (state.decision_tasks.token(), state.decisions.clone())
        };
        let owner = self.clone();
        let phase = self.phase.clone();
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
        Admission::Taken
    }

    /// The continuation limit, `Ok(None)` when no limit is set. A refused or
    /// unreadable document is logged and `Err(())`: the decision stops without
    /// an automatic input.
    async fn limit(&self) -> Result<Option<u64>, ()> {
        let snapshot = match self
            .settings
            .read(SOURCE, SettingsScope::User, &self.stop)
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
            let active = tokio::select! {
                active = previous.phase.activated() => active,
                _ = previous.stop.cancelled() => false,
            };
            if !active {
                return;
            }
            let active = tokio::select! {
                active = previous.session.activated() => active,
                _ = previous.stop.cancelled() => false,
            };
            if !active {
                return;
            }
            for info in previous.tree.list() {
                let Some(BranchSource::Host { operation_id }) = info.source else {
                    continue;
                };
                if info.parent.as_deref() != Some(previous.branch_id.as_str())
                    || info.create_anchor.is_none_or(|at| at > through)
                    || fold::evaluation_of_operation(&operation_id).is_none()
                {
                    continue;
                }
                let end = previous
                    .tree
                    .end(info.id, BranchSource::Host { operation_id });
                let result = tokio::select! {
                    result = end => result,
                    _ = previous.stop.cancelled() => return,
                };
                if let Err(error) = result
                    && !matches!(error, AgentError::BranchEnded(_))
                {
                    previous.dispatcher.fail(error);
                    return;
                }
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

    /// Writes the batch only while the branch's last assigned sequence is
    /// still `head` when it commits: a fact that committed behind it leaves
    /// nothing written, and the caller decides again (D3, C4, C5). A batch
    /// that carries a change's trigger answers a request from outside the
    /// branch, so it also passes the branch's input admission: a line that has
    /// ended, or is closing, takes no new input. A record without a trigger is
    /// the branch's own fact, which its close may still write. A record is a
    /// batch — the model's state input, the recovery event when `business`
    /// states a change (the evaluation request states none, A16), and the
    /// trigger — and `input_if_head` takes one input, so both conditions ride
    /// the batch's own registration under the commit lock instead.
    fn append_business_if_head(
        &self,
        inputs: Vec<InputMessageData>,
        mut business: Option<recovery::BusinessState>,
        trigger: Option<usize>,
        head: u64,
    ) -> Result<Option<Vec<Arc<AgentEvent>>>, AgentError> {
        let admitted = if trigger.is_some() {
            Some(self.tree.get(&self.branch_id)?)
        } else {
            None
        };
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
        if let Some(business) = business.as_mut() {
            if let Some(index) = trigger {
                business.input_id = drafts[index].id.clone();
            }
            drafts.push(business.draft()?);
        }
        // The sentinel error only cancels the batch; `moved` answers the
        // caller, which decides again from the fold that moved.
        let moved = std::cell::Cell::new(false);
        let events = self.timeline.append_batch_registered(drafts, |events| {
            if let Some(admitted) = &admitted {
                if admitted.is_closing() {
                    return Err(AgentError::Closed(self.branch_id.clone()));
                }
                if admitted.ended() {
                    return Err(AgentError::BranchEnded(self.branch_id.clone()));
                }
            }
            if self.timeline.committed() != head {
                moved.set(true);
                return Err(AgentError::Cancelled("Goal record superseded".into()));
            }
            let inputs_end = events.len() - usize::from(business.is_some());
            for event in &events[..inputs_end] {
                let EventKind::InputMessage(data) = &event.kind else {
                    unreachable!("before preserves input kind")
                };
                data.validate()
                    .map_err(|error| AgentError::InvalidInput(error.into()))?;
            }
            if let Some(business) = &business {
                let EventKind::Extension(actual) = &events[inputs_end].kind else {
                    unreachable!("before preserves extension kind")
                };
                if actual.key() != recovery::KEY
                    || recovery::BusinessState::decode(actual.data().clone())? != *business
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
                    let (objective, mut status, mut judge) =
                        state.fold.after_change(operation, &attrs);
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
            }
            self.state.lock().unwrap().last_write = events.last().expect("batch").sequence;
            Ok(())
        });
        match events {
            Ok(events) => Ok(Some(events)),
            Err(_) if moved.get() => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn pause_closing(&self) -> Result<(), AgentError> {
        let _write = self.write.lock().await;
        // A change committed before this lock can still be waiting for the
        // fold, and a record composed before it would state the state that
        // change replaced: the close catches up first, and states no pause
        // while the fold is behind.
        let Some(head) = self.catch_up().await? else {
            return Ok(());
        };
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
        #[cfg(feature = "testing")]
        crate::testing::stage_held(
            &self.session_id,
            &self.branch_id,
            crate::testing::Stage::Closing,
            head,
        )
        .await;
        let status = text::State::Paused("interrupted_unload".into());
        // The snapshot and the batch name the cutoff the fold was read at: a
        // fact that commits after catch-up leaves nothing written, and the load
        // mapping covers the missing pause.
        let input = text::state(
            &self.branch_id,
            objective.as_deref(),
            status.clone(),
            judge,
            text::Answered::Sequence(head),
            live,
            &self.texts,
        );
        let Some(events) = self.append_business_if_head(
            vec![input],
            Some(recovery::BusinessState::new(
                "pause",
                objective,
                status.clone(),
                judge,
                None,
            )),
            None,
            head,
        )?
        else {
            return Ok(());
        };
        // The pause is the fact that makes the evaluations unwanted, so their
        // lines end only once it committed (Pursuit §13 E2).
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
        #[cfg(feature = "testing")]
        if live {
            match &event.kind {
                EventKind::TurnStart(_) => {
                    crate::testing::fold_turn_start(&self.session_id, &self.branch_id, sequence)
                }
                EventKind::InputMessage(_) => {
                    crate::testing::fold_input(&self.session_id, &self.branch_id, sequence)
                }
                _ => {}
            }
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

    /// Takes what a cancelled background end or a withdrawn input released,
    /// once the branch is idle and no driving input is pending (A.4, Pursuit
    /// §5): a conclusion the wait deferred, once no task is open, or the
    /// turn-end decision, which the evaluator creation takes even with a task
    /// open. The cancelled status is read from the fact, so this cannot race a
    /// `completed` end's wake.
    fn settle_deferred(self: &Arc<Self>) {
        let pending = {
            let state = self.state.lock().unwrap();
            let fold = &state.fold;
            if !(fold.running() && !fold.turn_open() && !fold.pending_driving()) {
                return;
            }
            fold.pending_conclusion()
                .filter(|_| !fold.open_tasks())
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
                    .record(
                        at,
                        Some(text::State::Paused(reason.to_owned())),
                        Answers::Fact,
                    )
                    .await
                    .map(|_| ())
            })
        });
    }

    /// States the record and answers whether it ends the decision the caller
    /// holds: `false` means the fold no longer asked for it and the caller still
    /// decides the fact it named, and `true` means the record wrote the state, a
    /// newer state statement already answered the fact, or the branch took the
    /// decision the superseding fact wants (#2119).
    async fn record(
        self: &Arc<Self>,
        after: u64,
        status: Option<text::State>,
        answers: Answers,
    ) -> Result<bool, AgentError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let owner = self.clone();
        self.tasks.spawn(async move {
            let result = owner.record_inner(after, status, answers).await;
            #[cfg(feature = "testing")]
            crate::testing::stage_held(
                &owner.session_id,
                &owner.branch_id,
                crate::testing::Stage::Released,
                after,
            )
            .await;
            if let Err(Err(error)) = reply.send(result) {
                tracing::warn!(%error, "Goal terminal state could not be saved");
                let _ = owner.write_status.send_replace(Err(error));
                #[cfg(feature = "testing")]
                crate::testing::stage_held(
                    &owner.session_id,
                    &owner.branch_id,
                    crate::testing::Stage::Published,
                    after,
                )
                .await;
            }
        });
        result
            .await
            .map_err(|_| AgentError::Closed("Goal".into()))?
    }

    /// States `status` — or the pause the fold still decides for `answers` —
    /// as the answer to the fact at `after`, and answers whether its writer
    /// took that fact's decision. The record is composed only from a fold that
    /// has caught up to the branch's committed cutoff, and only while that fact
    /// still decides it: a driving input or a newer turn that folds while the
    /// decisions settle leaves the state to that turn (C4, C5, D3).
    ///
    /// A record the fold no longer asked for before the writer took the
    /// settlement answers `false` and touches nothing: it held no settlement,
    /// so it wakes no decision and takes up no interrupt a failed write left
    /// with the fold. A record the fold superseded after the writer took the
    /// settlement answers `true`: it released that settlement here, under the
    /// write guard, and asks for the decision the superseding fact still wants
    /// (#2119). A fact a newer state statement already answered answers `true`
    /// without waking anything.
    async fn record_inner(
        self: &Arc<Self>,
        after: u64,
        status: Option<text::State>,
        answers: Answers,
    ) -> Result<bool, AgentError> {
        #[cfg(feature = "testing")]
        crate::testing::stage_held(
            &self.session_id,
            &self.branch_id,
            crate::testing::Stage::Writer,
            after,
        )
        .await;
        let _write = self.write_guard().await?;
        {
            let state = self.state.lock().unwrap();
            // A newer state statement already answered the fact: the caller has
            // nothing left to decide, and this writer holds no settlement to
            // wake for it (#2119).
            if state.answered(after) {
                return Ok(true);
            }
            if state.stale(after, answers) {
                return Ok(false);
            }
        }
        let result = self.write_record(after, status, answers).await;
        match &result {
            // The settlement this record held is released here, under the
            // write guard that covered it, so no change can take a settlement
            // of its own between the release and the guard it waits for.
            Ok(true) => self.resume_settlement(),
            // A record the fold no longer asked for leaves the goal running
            // with a decision still due: a fact that committed while the
            // settlement held the decisions asked for one, and nothing else
            // takes it again.
            Ok(false) => {
                self.resume_settlement();
                if self.write_status.borrow().is_ok() {
                    self.resume_decisions();
                }
            }
            // A write that failed states nothing, so it leaves the interrupt
            // its settlement deferred with the fold for a later settlement;
            // the decisions that settlement cancelled decide nothing after
            // this release, because that settlement waited for every one of
            // them before it wrote (Pursuit §4.1, §13 E2, #2119).
            Err(_) => {
                self.release_settlement();
            }
        }
        result?;
        Ok(true)
    }

    /// States the record of the fact at `after` once the writer took the
    /// settlement: `Ok(true)` answered the fact, and `Ok(false)` met a fold
    /// that superseded it after the settlement and wrote nothing (C4, C5).
    async fn write_record(
        self: &Arc<Self>,
        after: u64,
        status: Option<text::State>,
        answers: Answers,
    ) -> Result<bool, AgentError> {
        self.settle_decisions().await?;
        self.write_status.borrow().clone()?;
        #[cfg(feature = "testing")]
        crate::testing::stage_held(
            &self.session_id,
            &self.branch_id,
            crate::testing::Stage::Record,
            after,
        )
        .await;
        loop {
            let through = self.await_dispatched().await?;
            let (objective, status, judge, live) = {
                let state = self.state.lock().unwrap();
                if self.timeline.committed() != through {
                    continue;
                }
                if state.stale(after, answers) {
                    return Ok(false);
                }
                let status = match status.clone() {
                    Some(status) => status,
                    None => match answers.pause(&state.fold, after) {
                        Some(reason) => text::State::Paused(reason.to_owned()),
                        None => state.fold.state.clone(),
                    },
                };
                (
                    state.fold.objective.clone(),
                    status,
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
            #[cfg(feature = "testing")]
            crate::testing::stage_held(
                &self.session_id,
                &self.branch_id,
                crate::testing::Stage::Append,
                after,
            )
            .await;
            if self
                .append_business_if_head(vec![input], Some(business), None, through)?
                .is_none()
            {
                continue;
            }
            let mut state = self.state.lock().unwrap();
            state.written.insert(after);
            state.recorded = state.recorded.max(after);
            return Ok(true);
        }
    }

    /// Waits until the fold has stepped the branch's committed cutoff and
    /// answers the cutoff it read. A closing branch states the state it ended
    /// on, so it reads the facts already committed before it composes its last
    /// record. `None` means the fold is still behind, and the caller writes
    /// nothing: the newest committed change stays in effect, and the load
    /// mapping covers the missing pause. The wait is bounded by the grace a
    /// closing branch gives its own delivery, and spent once: the branch runs
    /// its closing pause on both its lifecycle and its cleanup, and the second
    /// reads the fold as it stands instead of waiting again.
    async fn catch_up(&self) -> Result<Option<u64>, AgentError> {
        if self.state.lock().unwrap().closing_grace {
            let through = self.timeline.committed();
            return Ok((self.dispatcher.sequence() >= through).then_some(through));
        }
        let through = self.timeline.committed();
        let mut dispatched = self.dispatcher.watch();
        let stepped = async {
            while self.dispatcher.sequence() < through {
                if let Some(error) = self.dispatcher.failure() {
                    return Err(error);
                }
                if dispatched.changed().await.is_err() {
                    return Ok(None);
                }
            }
            Ok((self.timeline.committed() == through).then_some(through))
        };
        let result = tokio::time::timeout(CLOSING_GRACE, stepped)
            .await
            .unwrap_or(Ok(None));
        self.state.lock().unwrap().closing_grace = true;
        result
    }

    /// Ends the decisions this generation still runs and waits for the fold to
    /// take every write it made. It ends no evaluator: a line ends only from a
    /// folded fact that makes it unwanted ([`Self::settle`], `pause_closing`),
    /// never from a pause that has not committed yet (Pursuit §13 E2).
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
        self.await_fold().await
    }

    /// States the decisions a settlement refused to admit while it held task
    /// admission, before the record of a change that supersedes their facts:
    /// each names a fact older than that record, so the fold takes it only if
    /// its own record lands first (#2158). Answers the committed cutoff the
    /// last drain found the queue empty at, which the record that follows is
    /// conditioned on.
    async fn write_deferred(self: &Arc<Self>) -> Result<u64, AgentError> {
        loop {
            let through = self.await_dispatched().await?;
            let mut deferred = std::mem::take(&mut self.state.lock().unwrap().deferred);
            if deferred.is_empty() {
                return Ok(through);
            }
            while let Some(record) = deferred.pop_front() {
                let status = record.status.clone();
                if let Err(error) = self
                    .write_record(record.after, Some(status), record.answers)
                    .await
                {
                    // A record whose append failed writes nothing, so it and
                    // the ones not attempted after it stay for a later change,
                    // which states them or refuses them as superseded (#2158).
                    // The records admitted meanwhile name later facts.
                    let mut state = self.state.lock().unwrap();
                    deferred.push_front(record);
                    deferred.append(&mut state.deferred);
                    state.deferred = deferred;
                    return Err(error);
                }
            }
        }
    }

    /// States the newest change trigger, once (D3): the record names the
    /// trigger in `after`, so the fold and every client read the same state.
    fn answer_change(self: &Arc<Self>) {
        let Some(last) = self.state.lock().unwrap().fold.pending_change() else {
            return;
        };
        self.task(format!("state:{last}"), None, move |owner| {
            Box::pin(async move { owner.record(last, None, Answers::Change).await.map(|_| ()) })
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

    /// The evaluator creation for the turn ending at `after`, claimed only over
    /// a fold that has consumed every fact committed before the claim (C4, C5,
    /// Pursuit §4.1). The wait delivers the facts committed at the call, and the
    /// cutoff is read again under the fold lock: a Queue or a goal change that
    /// commits while the wait runs leaves that cutoff behind, so the claim
    /// catches up to the newer one before it reads the fold.
    async fn claim_evaluation(&self, after: u64) -> Result<Option<(String, String)>, AgentError> {
        loop {
            let through = self.await_dispatched().await?;
            let mut state = self.state.lock().unwrap();
            if self.timeline.committed() != through {
                continue;
            }
            if !fold::decide_evaluation(&state.fold, after) {
                return Ok(None);
            }
            let summary = state
                .fold
                .pending_request()
                .map(|request| request.summary.clone())
                .expect("an evaluation decision names a pending request");
            let objective = state.fold.objective.clone().unwrap_or_default();
            state.fold.start_evaluation(after);
            return Ok(Some((summary, objective)));
        }
    }

    /// Takes the evaluator-first decision for the turn ending at `after`
    /// (Pursuit §4.1): claims the evaluation and creates its line. Returns
    /// whether the decision was taken; `false` means the fold moved and the
    /// turn end is still open for another decision.
    async fn evaluate_turn_end(self: &Arc<Self>, after: u64) -> Result<bool, AgentError> {
        // A driving input or a goal change can commit after this turn end and
        // still be waiting to fold: the claim rechecks the end, the pending
        // request and the driving input over every committed fact (C4, C5,
        // Pursuit §4.1).
        let Some((summary, objective)) = self.claim_evaluation(after).await? else {
            return Ok(false);
        };
        #[cfg(feature = "testing")]
        crate::testing::stage_held(
            &self.session_id,
            &self.branch_id,
            crate::testing::Stage::Create,
            after,
        )
        .await;
        let cancel = self.stop.clone();
        let cancelled: Arc<dyn Fn() -> BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync> =
            Arc::new(move || {
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
                Answers::EvaluationFailed(after),
            )
            .await?;
        }
        self.settle();
        Ok(true)
    }

    /// The turn-end decision (A.4, Pursuit §4.1): an evaluator creation when
    /// the judge asks for one, otherwise the continuation limit.
    async fn do_turn_end(self: &Arc<Self>, after: u64) -> Result<(), AgentError> {
        let result = self.do_turn_end_inner(after).await;
        #[cfg(feature = "testing")]
        crate::testing::stage_held(
            &self.session_id,
            &self.branch_id,
            crate::testing::Stage::Done,
            after,
        )
        .await;
        result
    }

    async fn do_turn_end_inner(self: &Arc<Self>, after: u64) -> Result<(), AgentError> {
        loop {
            #[cfg(feature = "testing")]
            crate::testing::stage_held(
                &self.session_id,
                &self.branch_id,
                crate::testing::Stage::Decision,
                after,
            )
            .await;
            let evaluation = {
                let state = self.state.lock().unwrap();
                if state.fold.turn_end.as_ref().map(|end| end.sequence) != Some(after) {
                    return Ok(());
                }
                fold::decide_evaluation(&state.fold, after)
            };
            if evaluation && self.evaluate_turn_end(after).await? {
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
                    if !state.fold.turn_end_stands(after) {
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
                state.fold.turn_end_decision(after, limit, progress)
            };
            let Some(decision) = decision else {
                return Ok(());
            };
            #[cfg(feature = "testing")]
            crate::testing::write_held(&self.session_id, &self.branch_id, after).await;
            // C4, C5, #1989: a report input or a new turn that commits behind the
            // decision makes this continuation stale. The recheck reads the fold at
            // the latest committed cutoff, and the append refuses a head that moved
            // after that read. The recheck also returns the turn end to the
            // evaluator-first decision when a Queue that held it back was
            // withdrawn. A pause was decided above, at the writer's cutoff.
            match decision {
                Decision::Pause { .. } => {
                    drop(write);
                    // A record the fold no longer asked for settled nothing: the
                    // turn end the caller still owns decides again (#2119).
                    if self
                        .record(after, None, Answers::TurnEnd { limit, progress })
                        .await?
                    {
                        return Ok(());
                    }
                }
                Decision::Continuation { .. } => loop {
                    let through = self.await_dispatched().await?;
                    let (evaluation, decision) = {
                        let state = self.state.lock().unwrap();
                        if self.timeline.committed() != through {
                            continue;
                        }
                        (
                            fold::decide_evaluation(&state.fold, after),
                            state.fold.turn_end_decision(after, limit, progress),
                        )
                    };
                    if evaluation {
                        // A Queue that held the evaluator-first decision back was
                        // withdrawn: the turn end belongs to that decision again
                        // (Pursuit §4.1), not to this continuation.
                        drop(write);
                        self.evaluate_turn_end(after).await?;
                        return Ok(());
                    }
                    let Some(Decision::Continuation { count, idle }) = decision else {
                        return Ok(());
                    };
                    let input =
                        text::continuation(&self.branch_id, count, limit, idle, after, &self.texts);
                    if self.write_if_head(input, through).await?.is_some() {
                        return Ok(());
                    }
                },
            }
        }
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
        let paused = text::State::Paused("evaluation_failed".to_owned());
        let kept = paused.clone();
        let admission = self.task(format!("failed:{evaluation}"), None, move |owner| {
            Box::pin(async move {
                let current = owner.state.lock().unwrap().fold.is_current(evaluation);
                if !current {
                    return Ok(());
                }
                owner
                    .record(at, Some(paused), Answers::EvaluationFailed(evaluation))
                    .await
                    .map(|_| ())
            })
        });
        if admission == Admission::Held {
            self.keep_deferred(DeferredRecord {
                after: at,
                status: kept,
                answers: Answers::EvaluationFailed(evaluation),
            });
        }
    }

    /// The conclusion the evaluator wrote decides the evaluation it names
    /// (Pursuit §4.1). A conclusion the background wait deferred is taken
    /// again through here when the branch settles (Pursuit §5).
    fn conclude(self: &Arc<Self>, sequence: u64, conclusion: Conclusion) {
        let kept = conclusion.clone();
        let admission = self.task(
            format!("conclusion:{sequence}"),
            Some(Retry::Settle),
            move |owner| {
                Box::pin(async move {
                    let result = owner.run_conclusion(sequence, conclusion).await;
                    #[cfg(feature = "testing")]
                    crate::testing::stage_held(
                        &owner.session_id,
                        &owner.branch_id,
                        crate::testing::Stage::Done,
                        sequence,
                    )
                    .await;
                    result
                })
            },
        );
        if admission == Admission::Held
            && let Some(record) = self.report_record(sequence, &kept)
        {
            self.keep_deferred(record);
        }
    }

    /// The state record the conclusion at `sequence` states, if it still
    /// decides one: a `continue` conclusion states no record of its own, and
    /// a superseded one decides nothing (#2158). A `done` or `blocked`
    /// conclusion states the fact it decides: the background wait holds a
    /// `continue`, not a terminal conclusion (Pursuit §5), so its record is
    /// written while a task is open exactly as when it took admission.
    fn report_record(&self, sequence: u64, conclusion: &Conclusion) -> Option<DeferredRecord> {
        let state = self.state.lock().unwrap();
        let (status, answers) = match fold::decide_conclusion(&state.fold, sequence, conclusion)? {
            ConclusionAction::Completed(text) => (text::State::Completed(text), Answers::Fact),
            ConclusionAction::Blocked(text) => (text::State::Blocked(text), Answers::Fact),
            ConclusionAction::Failed => (
                text::State::Paused("evaluation_failed".to_owned()),
                Answers::Conclusion(conclusion.evaluation),
            ),
            ConclusionAction::Continue => return None,
        };
        Some(DeferredRecord {
            after: sequence,
            status,
            answers,
        })
    }

    fn keep_deferred(&self, record: DeferredRecord) {
        self.state.lock().unwrap().deferred.push_back(record);
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
                    Answers::Conclusion(evaluation),
                )
                .await?;
            }
            ConclusionAction::Completed(text) => {
                self.record(sequence, Some(text::State::Completed(text)), Answers::Fact)
                    .await?;
            }
            ConclusionAction::Blocked(text) => {
                self.record(sequence, Some(text::State::Blocked(text)), Answers::Fact)
                    .await?;
            }
            ConclusionAction::Continue => {
                #[cfg(feature = "testing")]
                crate::testing::stage_held(
                    &self.session_id,
                    &self.branch_id,
                    crate::testing::Stage::Continue,
                    sequence,
                )
                .await;
                let Ok(limit) = self.limit().await else {
                    return Ok(());
                };
                if self.stopped() {
                    return Ok(());
                }
                let current = {
                    let state = self.state.lock().unwrap();
                    fold::decide_conclusion(&state.fold, sequence, &conclusion)
                        == Some(ConclusionAction::Continue)
                        && state.fold.turn_end.as_ref().map(|end| end.sequence) == Some(evaluation)
                };
                if !current {
                    return Ok(());
                }
                // C7 measures the branch's own continuations; the evaluator
                // judged this turn, so this path reads no progress (Pursuit §5).
                // C4, C5: the decision is bound to the judged turn end, so a
                // Queue that opened a turn before or during the settings read
                // cannot pass as the expected one. The recheck reads the fold at
                // the latest committed cutoff and catches up to that Queue; a
                // turn the Queue opened leaves its turn open, a newer turn end
                // replaces the judged one, a turn interrupt after the judged end
                // pauses the goal, and the append refuses a head that moved
                // after the recheck. A pause is decided once, at the writer's
                // cutoff, and does not claim a Queue that commits after it.
                loop {
                    let write = self.write_guard().await?;
                    let decision = {
                        let state = self.state.lock().unwrap();
                        if state.written.contains(&sequence) {
                            return Ok(());
                        }
                        state
                            .fold
                            .evaluated_continuation(sequence, evaluation, limit)
                    };
                    match decision {
                        None => return Ok(()),
                        Some(Decision::Pause { .. }) => {
                            drop(write);
                            // A record the fold no longer asked for settled
                            // nothing: the conclusion the caller still owns
                            // decides again (#2119).
                            if self
                                .record(
                                    sequence,
                                    None,
                                    Answers::EvaluatedContinuation { evaluation, limit },
                                )
                                .await?
                            {
                                return Ok(());
                            }
                        }
                        Some(Decision::Continuation { .. }) => {
                            #[cfg(feature = "testing")]
                            crate::testing::write_held(
                                &self.session_id,
                                &self.branch_id,
                                evaluation,
                            )
                            .await;
                            loop {
                                let through = self.await_dispatched().await?;
                                let decision = {
                                    let state = self.state.lock().unwrap();
                                    if self.timeline.committed() != through {
                                        continue;
                                    }
                                    if state.written.contains(&sequence) {
                                        return Ok(());
                                    }
                                    state
                                        .fold
                                        .evaluated_continuation(sequence, evaluation, limit)
                                };
                                let Some(Decision::Continuation { count, idle }) = decision else {
                                    return Ok(());
                                };
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
                                if self.write_if_head(input, through).await?.is_some() {
                                    self.state.lock().unwrap().written.insert(sequence);
                                    return Ok(());
                                }
                            }
                        }
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
            let wanted = {
                let state = self.state.lock().unwrap();
                !state.fold.ended.contains(&evaluator)
                    && !state.fold.failed.contains(&evaluator)
                    && state.fold.wants_evaluator(evaluation)
            };
            if wanted {
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
                            || state.fold.wants_evaluator(evaluation)
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
