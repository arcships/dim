//! Goal's branch part (design §1, A.6, Pursuit §4): owns this branch's fold,
//! decides continuation from consumed state only, and writes through owned
//! tasks. On a branch Goal created for an evaluation it takes the evaluator
//! role instead (Pursuit §4.2).

use crate::fold::{self, Conclusion, ConclusionAction, Decision, Fold, TurnEnd};
use crate::text::{self, GoalTexts, Live};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    AgentError, BoxFuture, BranchLifecycle, BranchOptions, Dispatcher, Names, SessionLifecycle,
    Timeline, TimelineIdentity, TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy,
    TimelineQueryPredicate, TimelineQueryScope, rebuild_facts,
};
use dim_agent_types::{
    AgentEvent, BranchSource, EventKind, ExecStatus, InputBlock, InputMessageData, InterruptScope,
    TurnEndData, TurnOutcome,
};
use dim_app_server::settings::{SettingsScope, SettingsService};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

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

/// The loaded branch Owners, by branch id. The process plugin hands the same
/// table to its Session part and its branch parts, so a `session.goal.*` call
/// or the `goal_update` tool reaches the branch's Owner, which composes the
/// record, without a service (ADR 002 D3).
#[derive(Clone, Default)]
pub(crate) struct Changes {
    owners: Arc<Mutex<HashMap<String, Weak<Owner>>>>,
}

impl Changes {
    /// Writes `change` on `branch` through its Owner and returns the
    /// trigger's sequence.
    pub(crate) async fn change(&self, branch: &str, change: Change) -> Result<u64, AgentError> {
        let owner = self
            .owners
            .lock()
            .unwrap()
            .get(branch)
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::Reloading("Goal".into()))?;
        owner.change(change).await
    }
    /// Registers `owner` for its branch; the returned handle withdraws it.
    fn register(&self, owner: &Arc<Owner>) -> Weak<Owner> {
        let weak = Arc::downgrade(owner);
        self.owners
            .lock()
            .unwrap()
            .insert(owner.branch_id.clone(), weak.clone());
        weak
    }
    fn withdraw(&self, branch: &str, owner: &Weak<Owner>) {
        let mut owners = self.owners.lock().unwrap();
        if owners
            .get(branch)
            .is_some_and(|entry| Weak::ptr_eq(entry, owner))
        {
            owners.remove(branch);
        }
    }
}

/// The facts the fold consumes (A.6).
const KEYS: &[&str] = &[
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
/// Inputs one page of the new-goal scan keeps (D4).
const SCAN_PAGE: u64 = 256;

/// One cheap rebuild's scan (D4): the newest Goal block on the branch's own
/// segment, or the opening the scan reached without finding one.
struct SegmentScan {
    /// The newest local Goal block: its sequence and whether it is a retired
    /// state record.
    newest: Option<(u64, bool)>,
    /// The segment's opening: its sequence and whether it is a fork. Absent
    /// when a Goal block stopped the scan above it.
    opening: Option<(u64, bool)>,
    /// The read's cutoff.
    through: u64,
}

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
            let owner = Arc::new(Owner {
                branch_id: identity.branch_id.clone(),
                timeline,
                tree,
                settings,
                texts: self.texts.clone(),
                phase: phase.clone(),
                session: session.clone(),
                stop: stop.clone(),
                tasks: TaskTracker::new(),
                resolved,
                stepped,
                ready,
                state: Mutex::new(State::default()),
                write: tokio::sync::Mutex::new(()),
            });
            let registered = self.changes.register(&owner);
            let cleanup = owner.clone();
            let changes = self.changes.clone();
            ctx.effect(move || async move {
                cleanup.stop.cancel();
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
                changes.withdraw(&cleanup.branch_id, &registered);
                Ok(())
            })?;
            let closing = owner.clone();
            let lifecycle = phase.clone();
            owner.tasks.spawn(async move {
                tokio::select! {
                    _ = lifecycle.closing() => closing.stop.cancel(),
                    _ = closing.stop.cancelled() => {},
                }
            });
            // Who this branch is, and whether it is an evaluator line, is read
            // once and before any decision: every decision waits for it.
            let resolve = owner.clone();
            owner.tasks.spawn(async move {
                if resolve.phase.activated().await
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
                    KEYS.iter().map(|key| (*key).to_owned()).collect(),
                    Arc::new(move || {
                        let owner = rebuilding.clone();
                        Box::pin(async move { owner.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        let owner = consuming.clone();
                        Box::pin(async move {
                            owner.ingest(&event, true);
                            Ok(())
                        })
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
    /// The `after` values a rebuild's restatement named without answering the
    /// fact at them (R6.1): the cutoff can be a conclusion's own sequence.
    restated: std::collections::BTreeSet<u64>,
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
    tree: Arc<TreeCommands>,
    settings: Arc<SettingsService>,
    texts: Arc<GoalTexts>,
    phase: Arc<BranchLifecycle>,
    session: Arc<SessionLifecycle>,
    stop: CancellationToken,
    tasks: TaskTracker,
    /// Set once the branch's identity is read; every decision waits for it.
    resolved: watch::Sender<bool>,
    /// The sequence of the newest event the fold has stepped, so a change can
    /// wait for the fold's position before composing its record (R5).
    stepped: watch::Sender<u64>,
    /// Set once the rebuild has returned and the facts it held are folded
    /// (R6.2): a change composes only from that complete fold.
    ready: watch::Sender<bool>,
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
        self.await_ready().await?;
        let _write = self.write.lock().await;
        self.await_fold().await?;
        let trigger = change.trigger(&self.branch_id);
        let (objective, state, judge, live) = {
            let lock = self.state.lock().unwrap();
            let (attrs, op) =
                fold::change_of(&trigger).expect("a change carries its own trigger marker");
            let (objective, state, judge) = lock.fold.after_change(op, &attrs);
            (objective, state, judge, lock.fold.live)
        };
        let trigger_id = trigger
            .submission_id
            .clone()
            .expect("Goal inputs carry a submission id");
        let record = text::state(
            &self.branch_id,
            objective.as_deref(),
            state,
            judge,
            text::Answered::Trigger(&trigger_id),
            live,
            &self.texts,
        );
        let events = self
            .tree
            .inputs(self.branch_id.clone(), vec![record, trigger])
            .await?;
        let sequence = events
            .last()
            .expect("a change batch holds its trigger")
            .sequence;
        let mut lock = self.state.lock().unwrap();
        lock.recorded = lock.recorded.max(sequence);
        lock.written.insert(sequence);
        lock.last_write = lock.last_write.max(sequence);
        drop(lock);
        Ok(sequence)
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
        {
            let mut state = self.state.lock().unwrap();
            if !state.in_flight.insert(key.clone()) {
                if let Some(retry) = retry {
                    state.retry.insert(key, retry);
                }
                return;
            }
        }
        let owner = self.clone();
        let phase = self.phase.clone();
        self.tasks.spawn(async move {
            if phase.activated().await
                && !owner.stopped()
                && let Err(error) = work(owner.clone()).await
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

    /// Folds this branch's facts through the cutoff of one read, unpublished
    /// tail included, and releases only what lies above it (A.6). The read
    /// selects `KEYS`, the kinds the fold consumes, never the whole branch
    /// (ADR 020 D3). A branch with no goal of its own takes the cheap path
    /// first (D4): no goal, no 13-kind fold.
    async fn rebuild(self: &Arc<Self>) -> Result<u64, AgentError> {
        // An evaluator line needs its turn history (`turn_ends` tells its
        // first turn from a later one) and its own segment may carry no Goal
        // block, because its prompt lives in the `branch/create` that opened
        // it. Its identity is tree metadata, so it is read before the cheap
        // path; an unreadable identity folds, the safe side.
        let evaluator = match self.tree.info(self.branch_id.clone()) {
            Ok(info) => matches!(
                &info.source,
                Some(BranchSource::Host { operation_id })
                    if fold::evaluation_of_operation(operation_id).is_some()
            ),
            Err(_) => true,
        };
        if !evaluator && let Some(through) = self.rebuild_without_goal().await? {
            let start = self.state.lock().unwrap().start;
            if let Some(start) = start {
                self.decide_start(start);
            }
            return Ok(through);
        }
        let reply = rebuild_facts(&self.timeline, KEYS).await?;
        for event in &reply.events {
            self.ingest(event, false);
        }
        let through = reply.through;
        // The decisions the previous generation owed are taken once, before
        // any live event. A conclusion still pending from a cancelled end
        // takes the settle path: decide_end alone skips it, because the
        // evaluator's branch/create already decided the turn end.
        let pending = self
            .state
            .lock()
            .unwrap()
            .fold
            .pending_conclusion()
            .is_some();
        if pending {
            self.settle_deferred();
        } else {
            self.decide_end();
        }
        self.settle();
        let start = self.state.lock().unwrap().start;
        if let Some(start) = start {
            self.decide_start(start);
        }
        self.answer_change();
        self.reconcile(through).await?;
        Ok(through)
    }

    /// The cheap rebuild (D4): when the branch's own segment has no Goal block,
    /// or its newest one is a retired state record, the branch has no goal.
    /// Returns the cutoff live delivery continues from, or `None` when the
    /// full fold is owed.
    ///
    /// Nothing else Goal decides reads a fact that could exist here: every
    /// decision begins with `Fold::running`, which is false without an
    /// objective, and the settle pass needs an evaluation it cannot know
    /// without one. A fork still reads its copied prefix once, for the one
    /// thing the opening needs: which keys it left live (D5).
    async fn rebuild_without_goal(self: &Arc<Self>) -> Result<Option<u64>, AgentError> {
        let Some(scan) = self.newest_goal_block().await? else {
            return Ok(None);
        };
        if scan.newest.is_some_and(|(_, retired)| !retired) {
            return Ok(None);
        }
        // Only a branch with no local Goal block can owe the start clear, so
        // the copied prefix is read only then. That is also the only case that
        // reaches the opening: a scan that finds a Goal block stops above it.
        let copied_live = match scan.opening {
            Some((opening, true)) if scan.newest.is_none() => {
                Some(self.copied_keys(opening).await?)
            }
            _ => None,
        };
        let mut state = self.state.lock().unwrap();
        if let Some((opening, _)) = scan.opening {
            state.start = Some(opening);
        }
        state
            .fold
            .start_without_goal(copied_live, scan.newest.map(|(sequence, _)| sequence));
        Ok(Some(scan.through))
    }

    /// The newest Goal block on this branch's own segment and, when it holds
    /// none, the segment's opening (D4): one descending scan of the inputs from
    /// the committed head down to the opening, skipping inputs `input/deleted`
    /// removed. The scan stops at the newest Goal block or at the opening,
    /// whichever it meets first; a segment that holds no opening owes the full
    /// fold, and returns `None`.
    async fn newest_goal_block(&self) -> Result<Option<SegmentScan>, AgentError> {
        let mut deleted = HashSet::new();
        let mut from = None;
        let mut through;
        loop {
            let mut query = read_page(TimelineQueryScope::Branch, from, SCAN_PAGE, true);
            query.kinds = vec![
                "input/message".into(),
                "input/deleted".into(),
                "branch/start".into(),
                "session/forked".into(),
            ];
            query.direction = TimelineQueryDirection::Descending;
            let reply = self.timeline.query(query).await?;
            through = reply.through;
            for (index, event) in reply.events.iter().enumerate() {
                match &event.kind {
                    EventKind::InputDeleted(data) => {
                        deleted.insert(data.input_id.clone());
                    }
                    EventKind::InputMessage(data) if !deleted.contains(&event.id) => {
                        // R5: a change trigger states what the record
                        // immediately before it in the same batch states, so
                        // a `clear` batch takes the cheap path too. A pair the
                        // page boundary hides reads as the trigger alone: the
                        // full fold, the safe side.
                        let paired = fold::change_of(data)
                            .and_then(|_| reply.events.get(index + 1))
                            .and_then(|record| paired_record(record, data));
                        if let Some(retired) = paired.or_else(|| goal_record(data)) {
                            return Ok(Some(SegmentScan {
                                newest: Some((event.sequence, retired)),
                                opening: None,
                                through,
                            }));
                        }
                    }
                    EventKind::BranchStart(_) | EventKind::SessionForked(_) => {
                        return Ok(Some(SegmentScan {
                            newest: None,
                            opening: Some((
                                event.sequence,
                                matches!(event.kind, EventKind::SessionForked(_)),
                            )),
                            through,
                        }));
                    }
                    _ => {}
                }
            }
            let Some(last) = reply.events.last().map(|event| event.sequence) else {
                return Ok(None);
            };
            if (reply.events.len() as u64) < SCAN_PAGE || last <= 1 {
                return Ok(None);
            }
            from = Some(last - 1);
        }
    }

    /// Which Goal keys a fork's copied prefix left live (D5): one descending
    /// scan below the opening, the newest block of each key deciding.
    async fn copied_keys(&self, opening: u64) -> Result<Live, AgentError> {
        let mut live = Live::default();
        let mut found = (false, false, false);
        let mut deleted = HashSet::new();
        let mut from = opening.checked_sub(1).filter(|from| *from > 0);
        while let Some(start) = from {
            let mut query = read_page(TimelineQueryScope::Branch, Some(start), SCAN_PAGE, true);
            query.kinds = vec!["input/message".into(), "input/deleted".into()];
            query.direction = TimelineQueryDirection::Descending;
            let reply = self.timeline.query(query).await?;
            for event in &reply.events {
                match &event.kind {
                    EventKind::InputDeleted(data) => {
                        deleted.insert(data.input_id.clone());
                    }
                    EventKind::InputMessage(data) if !deleted.contains(&event.id) => {
                        for block in data.blocks.iter().rev() {
                            let InputBlock::Raw {
                                text: raw,
                                key,
                                retire,
                            } = block;
                            if text::marker(raw).is_none() {
                                continue;
                            }
                            let slot = match key.as_deref() {
                                Some(text::STATE_KEY) => &mut found.0,
                                Some(text::OBJECTIVE_KEY) => &mut found.1,
                                Some(text::STATUS_KEY) => &mut found.2,
                                _ => continue,
                            };
                            if *slot {
                                continue;
                            }
                            *slot = true;
                            match key.as_deref() {
                                Some(text::STATE_KEY) => live.state = !retire,
                                Some(text::OBJECTIVE_KEY) => live.objective = !retire,
                                _ => live.status = !retire,
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some(last) = reply.events.last().map(|event| event.sequence) else {
                break;
            };
            if found == (true, true, true) || (reply.events.len() as u64) < SCAN_PAGE || last <= 1 {
                break;
            }
            from = Some(last - 1);
        }
        Ok(live)
    }

    /// Folds one consumed local event and takes the decisions it carries. The
    /// rebuild's facts take decisions only through its closing steps.
    fn ingest(self: &Arc<Self>, event: &AgentEvent, live: bool) {
        let turn_start = self.state.lock().unwrap().fold.turn_start;
        // A deleted change trigger never took effect: the fold is recomputed
        // without it and, when a record naming it or a later change was
        // written, the recomputed state is stated again (R2). The sequence is
        // read before the fold drops the trigger.
        let withdrawn = match &event.kind {
            EventKind::InputDeleted(data) => self
                .state
                .lock()
                .unwrap()
                .fold
                .change_trigger(&data.input_id),
            _ => None,
        };
        self.state.lock().unwrap().fold.apply(event);
        let sequence = event.sequence;
        self.stepped.send_replace(sequence);
        if fold::is_opening(&event.kind) {
            self.state.lock().unwrap().start = Some(sequence);
            if live {
                self.decide_start(sequence);
            }
            return;
        }
        if !live {
            return;
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
            EventKind::InputDeleted(_) => {
                self.decide_end();
                if let Some(withdrawn) = withdrawn {
                    self.answer_deletion(sequence, withdrawn);
                }
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
                let _write = owner.write.lock().await;
                let write = {
                    let mut state = owner.state.lock().unwrap();
                    let write = !state.fold.is_decided(at)
                        && state.fold.local_blocks == 0
                        && state.fold.last_change == 0;
                    if write {
                        state.recorded = state.recorded.max(at);
                    }
                    write
                };
                if !write {
                    return Ok(());
                }
                owner
                    .write(text::start_clear(&owner.branch_id, at, live))
                    .await?;
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
                        false,
                        None,
                    )
                    .await
            })
        });
    }

    /// Writes one state record for `after` (D2, D3, R2, R4): the state the
    /// fold states now, or `state` when the write decides a new status (a
    /// pause, a completion, a failure). Serialized with every other record,
    /// skipped when a newer change trigger asked for another state and when a
    /// record with a higher `after` was already composed, so the newest record
    /// always states the newest state. The old keys the fold still holds live
    /// are retired in the same input (R1). `answer` marks the write as the
    /// answer to the newest pending change: it is skipped when another record
    /// already answered that change, as a deletion does. `withdrawn` marks the
    /// write as the compensation for a deleted change trigger: it is written
    /// only when a record naming that trigger, or a later record, was already
    /// written or is being written, decided here under the write lock.
    /// `restate` marks the write as a rebuild's restatement of the state (R6.1)
    /// rather than an answer: it names the rebuild's cutoff in `after`, which
    /// can be a conclusion's or a turn end's own sequence, so it claims no
    /// fact as answered. A restatement yields to any record (R7.1): it raises
    /// no ordering mark (`recorded`), it is composed only once every Goal
    /// write this generation made has been folded, and it is written only
    /// when no Goal write committed above its cutoff — a later write was
    /// composed from the rebuilt fold and already replaced the block.
    async fn record(
        self: &Arc<Self>,
        after: u64,
        state: Option<text::State>,
        answer: bool,
        withdrawn: Option<u64>,
    ) -> Result<(), AgentError> {
        self.record_marked(after, state, answer, withdrawn, false)
            .await
    }

    async fn record_marked(
        self: &Arc<Self>,
        after: u64,
        state: Option<text::State>,
        answer: bool,
        withdrawn: Option<u64>,
        restate: bool,
    ) -> Result<(), AgentError> {
        let _write = self.write.lock().await;
        if restate {
            // The statement must be the fold as it stands after every Goal
            // write this generation made (R7.1); a record composed from a
            // fold that has not stepped them states an older state and would
            // publish it over the record that already states the newer one.
            self.await_fold().await?;
        }
        let (objective, state, judge, live) = {
            let mut lock = self.state.lock().unwrap();
            if answer && lock.fold.pending_change() != Some(after) {
                return Ok(());
            }
            if let Some(withdrawn) = withdrawn {
                let named = lock.fold.recorded_at_or_after(withdrawn)
                    || lock.written.range(withdrawn..).next().is_some();
                if !named {
                    return Ok(());
                }
            }
            if lock.fold.last_change > after || after < lock.recorded {
                return Ok(());
            }
            if restate {
                // A write above the cutoff replaced the published block with
                // a state composed from the rebuilt fold (R7.1).
                if lock.last_write > after || !lock.restated.insert(after) {
                    return Ok(());
                }
            } else {
                if !lock.written.insert(after) {
                    return Ok(());
                }
                lock.recorded = after;
            }
            let fold = &lock.fold;
            (
                fold.objective.clone(),
                state.clone().unwrap_or_else(|| fold.state.clone()),
                fold.judge,
                fold.live,
            )
        };
        let answered = if restate {
            text::Answered::Restated(after)
        } else {
            text::Answered::Sequence(after)
        };
        let input = text::state(
            &self.branch_id,
            objective.as_deref(),
            state,
            judge,
            answered,
            live,
            &self.texts,
        );
        if let Err(error) = self.write(input).await {
            let mut lock = self.state.lock().unwrap();
            if restate {
                lock.restated.remove(&after);
            } else {
                lock.written.remove(&after);
            }
            return Err(error);
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
            Box::pin(async move { owner.record(last, None, true, None).await })
        });
    }

    /// R6.1: the published `goal:state` block and the fold's state agree. A
    /// rebuild can find the newest published record naming a change the fold
    /// dropped — a deletion whose compensating record never committed — so it
    /// writes one record stating the recomputed state. Once that record
    /// commits, the two agree and a later load writes nothing.
    async fn reconcile(self: &Arc<Self>, through: u64) -> Result<(), AgentError> {
        let published = self.published_state(through).await?;
        let (pending, statement, old_keys_live) = {
            let lock = self.state.lock().unwrap();
            (
                lock.fold.pending_change().is_some(),
                statement_of_fold(&lock.fold),
                lock.fold.live.any(),
            )
        };
        // The trigger the answer path owes is stated by that answer itself;
        // composing the same state twice would write two records.
        if pending {
            return Ok(());
        }
        let agrees = match &published {
            // A history written before the state record publishes its state
            // under the old keys: the keyed read states the same goal and the
            // fold states it, so the branch owes no record at load.
            None => statement.retired || old_keys_live,
            Some((raw, retire)) => published_statement(raw, *retire).as_ref() == Some(&statement),
        };
        if agrees {
            return Ok(());
        }
        // A record cannot commit before the branch is published: `TreeCommands`
        // resolves a branch through the tree's owned contexts, which exist
        // only once its build returned (`TimelineTree::get`,
        // crates/dim-agent-capabilities/src/timeline_tree.rs:118). Every
        // record path writes in a task that waits `phase.activated()` for
        // exactly this reason, and the guards in `record` still apply there.
        self.task(format!("reconcile:{through}"), None, move |owner| {
            Box::pin(async move { owner.record_marked(through, None, false, None, true).await })
        });
        Ok(())
    }

    /// The newest `goal:state` block on the branch's own segment at `through`,
    /// resolved by the keyed-block rule `session.keys` serves (keyed-blocks
    /// §2.3; `scan_keys` at
    /// crates/dim-app-server/src/connection/tree.rs:707): descending inputs, an
    /// `input/deleted` removes its input, the newest block of the key inside one
    /// input wins, and a `retire` block ends the key. Returns the raw block text
    /// and its retire flag, or `None` when no block of the key is above the
    /// opening.
    async fn published_state(&self, through: u64) -> Result<Option<(String, bool)>, AgentError> {
        if through == 0 {
            // An empty segment publishes no block, and a read cannot start
            // below the first sequence.
            return Ok(None);
        }
        let mut deleted = HashSet::new();
        let mut from = Some(through);
        while let Some(below) = from {
            let mut query = read_page(TimelineQueryScope::Branch, Some(below), SCAN_PAGE, true);
            query.kinds = vec![
                "input/message".into(),
                "input/deleted".into(),
                "branch/start".into(),
                "session/forked".into(),
            ];
            query.direction = TimelineQueryDirection::Descending;
            let reply = self.timeline.query(query).await?;
            for event in &reply.events {
                match &event.kind {
                    EventKind::BranchStart(_) | EventKind::SessionForked(_) => return Ok(None),
                    EventKind::InputDeleted(data) => {
                        deleted.insert(data.input_id.clone());
                    }
                    EventKind::InputMessage(data) if !deleted.contains(&event.id) => {
                        for block in data.blocks.iter().rev() {
                            let InputBlock::Raw {
                                text: raw,
                                key: Some(key),
                                retire,
                            } = block
                            else {
                                continue;
                            };
                            if key == text::STATE_KEY {
                                return Ok(Some((raw.clone(), *retire)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some(last) = reply.events.last().map(|event| event.sequence) else {
                return Ok(None);
            };
            if (reply.events.len() as u64) < SCAN_PAGE || last <= 1 {
                return Ok(None);
            }
            from = Some(last - 1);
        }
        Ok(None)
    }

    /// States the branch again after the deletion of the change trigger
    /// `withdrawn` (R2): the trigger is gone from the fold and a record that
    /// named it states a state the branch no longer has, so the recomputed
    /// state is stated with the deletion's sequence. Whether such a record
    /// exists is decided under the write lock.
    fn answer_deletion(self: &Arc<Self>, at: u64, withdrawn: u64) {
        self.task(format!("delete:{at}"), None, move |owner| {
            Box::pin(async move { owner.record(at, None, false, Some(withdrawn)).await })
        });
    }

    /// Whether this branch already committed its `branch/create` for `after`:
    /// from then on the creation records its own outcome, so a failed wait
    /// only means the caller stopped waiting (Pursuit §4.1).
    async fn created(&self, after: u64) -> Result<bool, AgentError> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                kinds: vec!["branch/create".into()],
                predicate: Some(TimelineQueryPredicate {
                    path: "source.operation_id".into(),
                    value: fold::evaluation_operation(after).into(),
                }),
                limit: NonZeroU64::new(1).expect("one fact is a page"),
                ..read_page(TimelineQueryScope::Branch, Some(after), 1, true)
            })
            .await?;
        Ok(!reply.events.is_empty())
    }

    /// Whether the turn `turn_id` made a tool call: one bounded query from the
    /// turn's start, limit one (A.4 C7). The kind is not folded (A.6).
    async fn turn_had_tool_call(&self, turn_start: u64, turn_id: &str) -> Result<bool, AgentError> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                kinds: vec!["step/tool-call".into()],
                predicate: Some(TimelineQueryPredicate {
                    path: "turn_id".into(),
                    value: turn_id.into(),
                }),
                limit: NonZeroU64::new(1).expect("one fact is a page"),
                ..read_page(TimelineQueryScope::Branch, Some(turn_start), 1, true)
            })
            .await?;
        Ok(!reply.events.is_empty())
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
            if self
                .tree
                .create(self.branch_id.clone(), after, options, cancelled)
                .await
                .is_err()
                && !self.stopped()
                && !self.created(after).await?
                && self.state.lock().unwrap().fold.is_current(after)
            {
                self.record(
                    after,
                    Some(text::State::Paused("evaluation_failed".to_owned())),
                    false,
                    None,
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
                if state.fold.turn_end.as_ref().map(|end| end.sequence) != Some(after) {
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
        let decision = {
            let state = self.state.lock().unwrap();
            if state.fold.turn_end.as_ref().map(|end| end.sequence) != Some(after) {
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
                self.record(
                    after,
                    Some(text::State::Paused(reason.to_owned())),
                    false,
                    None,
                )
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
        let mut events = Vec::new();
        let mut from = turn_start;
        loop {
            let page = self
                .timeline
                .query(TimelineQuery {
                    kinds: vec!["step/model-start".into(), "step/text".into()],
                    predicate: Some(TimelineQueryPredicate {
                        path: "turn_id".into(),
                        value: turn_id.into(),
                    }),
                    ..read_page(TimelineQueryScope::Branch, Some(from), REPLY_PAGE, false)
                })
                .await?;
            events.extend(
                page.events
                    .iter()
                    .filter(|event| event.sequence <= through)
                    .cloned(),
            );
            let Some(last) = page.events.last() else {
                break;
            };
            if (page.events.len() as u64) < REPLY_PAGE || last.sequence >= through {
                break;
            }
            from = last.sequence + 1;
        }
        let mut request = None;
        for event in &events {
            if let EventKind::ModelStart(data) = &event.kind
                && data.turn_id == turn_id
            {
                request = Some(data.request_id.clone());
            }
        }
        let Some(request) = request else {
            return Ok(String::new());
        };
        let mut reply = String::new();
        for event in &events {
            if let EventKind::Text(data) = &event.kind
                && data.turn_id == turn_id
                && data.request_id == request
            {
                reply.push_str(&data.delta);
            }
        }
        Ok(reply)
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
                        None,
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
                    None,
                )
                .await?;
            }
            ConclusionAction::Completed(text) => {
                self.record(sequence, Some(text::State::Completed(text)), false, None)
                    .await?;
            }
            ConclusionAction::Blocked(text) => {
                self.record(sequence, Some(text::State::Blocked(text)), false, None)
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
                        if !self.state.lock().unwrap().written.insert(sequence) {
                            return Ok(());
                        }
                        if let Err(error) = self.write(input).await {
                            self.state.lock().unwrap().written.remove(&sequence);
                            return Err(error);
                        }
                    }
                    Decision::Pause { reason } => {
                        self.record(
                            sequence,
                            Some(text::State::Paused(reason.to_owned())),
                            false,
                            None,
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
        let mut found = (false, false, false);
        for (branch, cut) in self.timeline.ancestors().iter().rev() {
            // A cut of zero contributes nothing to the chain (the chain read
            // skips it), and a read cannot start at sequence zero.
            if *cut == 0 {
                continue;
            }
            let mut from = *cut;
            loop {
                let mut query = read_page(
                    TimelineQueryScope::Branch,
                    Some(from),
                    INHERITED_PAGE,
                    false,
                );
                query.kinds = vec!["input/message".into()];
                query.direction = TimelineQueryDirection::Descending;
                let reply = self.tree.query(branch.clone(), query).await?;
                for event in &reply.events {
                    let EventKind::InputMessage(data) = &event.kind else {
                        continue;
                    };
                    for block in data.blocks.iter().rev() {
                        let InputBlock::Raw {
                            text: block_text,
                            key,
                            retire,
                        } = block;
                        if text::marker(block_text).is_none() {
                            continue;
                        }
                        let (slot, value) = match key.as_deref() {
                            Some(text::STATE_KEY) => (&mut found.0, &mut live.state),
                            Some(text::OBJECTIVE_KEY) => (&mut found.1, &mut live.objective),
                            Some(text::STATUS_KEY) => (&mut found.2, &mut live.status),
                            _ => continue,
                        };
                        if *slot {
                            continue;
                        }
                        *slot = true;
                        *value = !retire;
                    }
                }
                if found == (true, true, true) {
                    return Ok(live);
                }
                let Some(last) = reply.events.last().map(|event| event.sequence) else {
                    break;
                };
                if (reply.events.len() as u64) < INHERITED_PAGE || last <= 1 {
                    break;
                }
                from = last - 1;
            }
        }
        Ok(live)
    }
}

/// Whether `record`, the event immediately before the change trigger `data`,
/// is the state record that names `data` by submission id; `Some(retired)`
/// then carries the record's classification (R5).
fn paired_record(record: &AgentEvent, data: &InputMessageData) -> Option<bool> {
    let EventKind::InputMessage(record) = &record.kind else {
        return None;
    };
    let trigger = data.submission_id.as_deref()?;
    for block in &record.blocks {
        let InputBlock::Raw {
            text: raw,
            key,
            retire,
        } = block;
        if key.as_deref() != Some(text::STATE_KEY) {
            continue;
        }
        let attrs = text::marker(raw)?;
        return (attrs.get("trigger").map(String::as_str) == Some(trigger)).then_some(*retire);
    }
    None
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

/// The state the fold computes, normalised as `text::state` writes it: no
/// objective is a retired record, whatever the fold still holds.
fn statement_of_fold(fold: &Fold) -> Statement {
    let objective = fold
        .objective
        .clone()
        .filter(|objective| !objective.is_empty());
    let Some(objective) = objective else {
        return Statement {
            objective: None,
            state: text::State::None,
            judge: text::Judge::Model,
            retired: true,
        };
    };
    Statement {
        objective: Some(objective),
        state: fold.state.clone(),
        judge: fold.judge,
        retired: false,
    }
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

/// Whether `data` carries a Goal block, and whether it states a retired state
/// record (D4). Any marker counts: an evaluator's prompt and a continuation
/// are Goal's blocks too, and a branch that has them has history the fold must
/// read. An input with a `goal:state` block is classified by that block: the
/// old-key retires that ride with it belong to its statement (R3).
fn goal_record(data: &dim_agent_types::InputMessageData) -> Option<bool> {
    let mut record = None;
    for block in &data.blocks {
        let InputBlock::Raw {
            text: raw,
            key,
            retire,
        } = block;
        if text::marker(raw).is_none() {
            continue;
        }
        if key.as_deref() == Some(text::STATE_KEY) {
            return Some(*retire);
        }
        record = Some(false);
    }
    record
}

/// One page of this branch's facts, oldest first from `from` in ascending
/// reads; the direction is the caller's.
fn read_page(
    scope: TimelineQueryScope,
    from: Option<u64>,
    limit: u64,
    include_publishing: bool,
) -> TimelineQuery {
    TimelineQuery {
        scope,
        from_event: None,
        from_sequence: from,
        order_by: TimelineQueryOrderBy::Sequence,
        direction: TimelineQueryDirection::Ascending,
        include_publishing,
        kinds: Vec::new(),
        flags: Vec::new(),
        event_id: None,
        predicate: None,
        limit: NonZeroU64::new(limit).expect("a page keeps at least one fact"),
        offset: 0,
    }
}

fn result_name(result: fold::ConclusionKind) -> &'static str {
    match result {
        fold::ConclusionKind::Done => "done",
        fold::ConclusionKind::Continue => "continue",
        fold::ConclusionKind::Blocked => "blocked",
        fold::ConclusionKind::Failed => "failed",
    }
}
