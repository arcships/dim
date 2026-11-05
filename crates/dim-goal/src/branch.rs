//! Goal's branch part (design §1, A.6, Pursuit §4): owns this branch's fold,
//! decides continuation from consumed state only, and writes through owned
//! tasks. On a branch Goal created for an evaluation it takes the evaluator
//! role instead (Pursuit §4.2).

use crate::fold::{self, Conclusion, ConclusionAction, Decision, Fold, TurnEnd};
use crate::text::{self, GoalTexts};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    AgentError, BoxFuture, BranchLifecycle, BranchOptions, Dispatcher, Names, PredicateOp,
    SessionLifecycle, Timeline, TimelineIdentity, TimelineQuery, TimelineQueryDirection,
    TimelineQueryOrderBy, TimelineQueryPredicate, TimelineQueryScope, rebuild_facts,
};
use dim_agent_types::{
    AgentEvent, BranchSource, EventKind, ExecStatus, InputBlock, InterruptScope, TurnEndData,
    TurnOutcome,
};
use dim_app_server::settings::{SettingsScope, SettingsService};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const SOURCE: &str = "goal";

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
}

impl GoalBranch {
    pub fn new(names: &Names<'_>, texts: Arc<GoalTexts>) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            timeline: names.key()?,
            tree: names.key()?,
            settings: names.key()?,
            identity: names.key()?,
            phase: names.key()?,
            session: names.key()?,
            texts,
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
                state: Mutex::new(State::default()),
            });
            let cleanup = owner.clone();
            ctx.effect(move || async move {
                cleanup.stop.cancel();
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
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
    /// The evaluation whose `continue` conclusion Goal has answered with a
    /// write the fold has not consumed yet. A retry that runs before that
    /// input is folded must not answer the same evaluation again. A newer
    /// turn end or an operator input clears it.
    answered: Option<u64>,
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
    state: Mutex<State>,
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
    /// (ADR 020 D3).
    async fn rebuild(self: &Arc<Self>) -> Result<u64, AgentError> {
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
        Ok(through)
    }

    /// Folds one consumed local event and takes the decisions it carries. The
    /// rebuild's facts take decisions only through its closing steps.
    fn ingest(self: &Arc<Self>, event: &AgentEvent, live: bool) {
        let turn_start = self.state.lock().unwrap().fold.turn_start;
        self.state.lock().unwrap().fold.apply(event);
        let sequence = event.sequence;
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
                    self.evaluation_failed(evaluation);
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
                        self.evaluation_failed(evaluation);
                    }
                }
            }
            EventKind::TurnEnd(data) => {
                self.state.lock().unwrap().answered = None;
                self.turn_decision(sequence, turn_start, data.clone())
            }
            EventKind::StopInterrupt(data) if data.scope == InterruptScope::Turn => {
                self.interrupted(sequence)
            }
            EventKind::InputMessage(data) => {
                let operator = data.user.is_some()
                    && !data.blocks.first().is_some_and(|block| match block {
                        InputBlock::Raw { text, .. } => text.starts_with("<dim-"),
                    });
                if operator {
                    self.state.lock().unwrap().answered = None;
                }
                if let Some(conclusion) = data.blocks.first().and_then(|block| match block {
                    InputBlock::Raw { text, .. } => fold::conclusion_of(text),
                }) {
                    self.conclude(sequence, conclusion);
                }
                self.settle();
            }
            EventKind::InputDeleted(_) => self.decide_end(),
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
            if live {
                self.started_clear(at);
            }
            return;
        }
        self.task(format!("start:{at}"), None, move |owner| {
            Box::pin(async move {
                if owner.inherited_live().await? {
                    owner.started_clear(at);
                }
                Ok(())
            })
        });
    }

    fn started_clear(self: &Arc<Self>, at: u64) {
        self.task(format!("clear:{at}"), None, move |owner| {
            Box::pin(async move {
                let write = {
                    let state = owner.state.lock().unwrap();
                    !state.fold.is_decided(at) && state.fold.local_blocks == 0
                };
                if !write {
                    return Ok(());
                }
                owner
                    .tree
                    .input(
                        owner.branch_id.clone(),
                        text::start_clear(&owner.branch_id, at),
                    )
                    .await?;
                Ok(())
            })
        });
    }

    fn interrupted(self: &Arc<Self>, at: u64) {
        self.task(format!("interrupt:{at}"), None, move |owner| {
            Box::pin(async move {
                let decision = {
                    let state = owner.state.lock().unwrap();
                    fold::decide_interrupt(&state.fold, at)
                };
                if let Some(Decision::Pause { reason }) = decision {
                    owner
                        .tree
                        .input(
                            owner.branch_id.clone(),
                            text::automatic_pause(&owner.branch_id, reason, at),
                        )
                        .await?;
                }
                Ok(())
            })
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
                    op: PredicateOp::Eq,
                    path: "source.operation_id".into(),
                    value: fold::evaluation_operation(after).into(),
                }),
                predicates: Vec::new(),
                text: None,
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
                self.tree
                    .input(
                        self.branch_id.clone(),
                        text::automatic_pause(&self.branch_id, "evaluation_failed", after),
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
        let input = match decision {
            Decision::Continuation { count, idle } => {
                text::continuation(&self.branch_id, count, limit, idle, after, &self.texts)
            }
            Decision::Pause { reason } => text::automatic_pause(&self.branch_id, reason, after),
        };
        self.tree.input(self.branch_id.clone(), input).await?;
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
            self.tree
                .input(
                    self.branch_id.clone(),
                    text::evaluate_again(&self.branch_id, sequence),
                )
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
                        op: PredicateOp::Eq,
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

    fn evaluation_failed(self: &Arc<Self>, evaluation: u64) {
        self.task(format!("failed:{evaluation}"), None, move |owner| {
            Box::pin(async move {
                let current = owner.state.lock().unwrap().fold.is_current(evaluation);
                if !current {
                    return Ok(());
                }
                owner
                    .tree
                    .input(
                        owner.branch_id.clone(),
                        text::automatic_pause(&owner.branch_id, "evaluation_failed", evaluation),
                    )
                    .await?;
                Ok(())
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
        // The write for this evaluation is committed before the fold consumes
        // it; a retry that runs in between must not answer it again.
        if self.state.lock().unwrap().answered == Some(evaluation) {
            return Ok(());
        }
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
                self.tree
                    .input(
                        self.branch_id.clone(),
                        text::automatic_pause(&self.branch_id, "evaluation_failed", evaluation),
                    )
                    .await?;
            }
            ConclusionAction::Completed(text) => {
                self.tree
                    .input(
                        self.branch_id.clone(),
                        text::evaluated(&self.branch_id, "completed", &named, &text),
                    )
                    .await?;
            }
            ConclusionAction::Blocked(text) => {
                self.tree
                    .input(
                        self.branch_id.clone(),
                        text::evaluated(&self.branch_id, "blocked", &named, &text),
                    )
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
                    let mut state = self.state.lock().unwrap();
                    if state.answered == Some(evaluation)
                        || fold::decide_conclusion(&state.fold, sequence, &conclusion)
                            != Some(ConclusionAction::Continue)
                        || state.fold.turn_end.as_ref().map(|end| end.sequence)
                            != Some(end.sequence)
                    {
                        return Ok(());
                    }
                    let decision = fold::decide_evaluated_continuation(&state.fold, limit);
                    if decision.is_some() {
                        // The write is committed before the fold consumes it;
                        // a retry in between must not answer this evaluation
                        // again.
                        state.answered = Some(evaluation);
                    }
                    decision
                };
                let Some(decision) = decision else {
                    return Ok(());
                };
                let input = match decision {
                    Decision::Continuation { count, idle } => text::evaluated_continuation(
                        &self.branch_id,
                        count,
                        limit,
                        idle,
                        evaluation,
                        &named,
                    ),
                    Decision::Pause { reason } => {
                        text::automatic_pause(&self.branch_id, reason, evaluation)
                    }
                };
                if let Err(error) = self.tree.input(self.branch_id.clone(), input).await {
                    self.state.lock().unwrap().answered = None;
                    return Err(error);
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

    /// The goal an inherited prefix left live, read newest first one ancestor
    /// at a time, if the branch has no copied facts to answer it (A11).
    ///
    /// `Timeline::ancestors` is root first and the direct parent is pushed
    /// after that list (`timeline_tree.rs`, `restore` and `branch`), so the
    /// reverse is nearest first: the first keyed block found is the nearest
    /// one, which is the one the chain read returned before.
    async fn inherited_live(&self) -> Result<bool, AgentError> {
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
                    let mut live = None;
                    for block in &data.blocks {
                        let InputBlock::Raw {
                            text: block_text,
                            key,
                            retire,
                        } = block;
                        if text::marker(block_text).is_some()
                            && key.as_deref().is_some_and(|key| {
                                key == text::OBJECTIVE_KEY || key == text::STATUS_KEY
                            })
                        {
                            live = Some(!retire);
                        }
                    }
                    if let Some(live) = live {
                        return Ok(live);
                    }
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
        Ok(false)
    }
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
        predicates: Vec::new(),
        text: None,
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
