//! Goal's language-neutral fold and decisions (design §2, appendix A.4, B).
//!
//! Current execution consumes Goal commands, model notifications and terminal
//! facts. Reload installs the latest structured `goal/state`, pauses unfinished
//! work and does not replay old model text or execution progress.

use crate::text::{self, Judge, Marker, STATE_KEY};
use dim_agent_types::{
    AgentEvent, BranchSource, EventKind, InputBlock, InputMode, InterruptScope, TurnOutcome,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub sequence: u64,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnEnd {
    pub sequence: u64,
    pub status: TurnStatus,
    /// The turn's id, which a progress query names (C7).
    pub turn_id: String,
    /// The `turn/start` sequence the end closes; a progress query starts here.
    pub turn_start: u64,
    /// Whether Goal's own continuation drove the turn that ended here.
    pub continuation_driven: bool,
}

/// Whether the turn a decision follows made a tool call (C7). `None` where the
/// decision needs the answer means it is not known yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    ToolCall,
    NoToolCall,
}

/// Consecutive continuation-driven turns without a tool call that pause a goal
/// (C7).
pub const NO_PROGRESS_TURNS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

pub enum TurnStatus {
    Completed,
    Failed,
    Cancelled,
}

impl TurnStatus {
    fn of(outcome: &TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed { .. } => Self::Completed,
            TurnOutcome::Failed { .. } => Self::Failed,
            TurnOutcome::Cancelled { .. } => Self::Cancelled,
        }
    }
}

/// The result a conclusion states, and the evaluator's report of a failed turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]

pub enum ConclusionKind {
    Done,
    Continue,
    Blocked,
    Failed,
}

impl ConclusionKind {
    fn of(value: &str) -> Option<Self> {
        match value {
            "done" => Some(Self::Done),
            "continue" => Some(Self::Continue),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Done => "DONE:",
            Self::Continue => "CONTINUE:",
            Self::Blocked => "BLOCKED:",
            Self::Failed => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conclusion {
    pub evaluation: u64,
    pub evaluator: String,
    pub result: ConclusionKind,
    pub text: String,
}

impl Conclusion {
    /// The result a `done` or `blocked` conclusion states, without its prefix.
    pub fn result_text(&self) -> String {
        let prefix = self.result.prefix().encode_utf16().count();
        let units: Vec<u16> = self.text.encode_utf16().skip(prefix).collect();
        String::from_utf16_lossy(&units).trim().to_owned()
    }
}

/// A sequence as Goal writes one: decimal, without a leading zero.
pub fn sequence_of(value: &str) -> Option<u64> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// The operation id a `branch/create` records for evaluation `N` (§4.1).
pub fn evaluation_operation(evaluation: u64) -> String {
    format!("goal:evaluate:{evaluation}")
}

/// The evaluation a `branch/create` operation id names, if it is one.
pub fn evaluation_of_operation(operation: &str) -> Option<u64> {
    sequence_of(operation.strip_prefix("goal:evaluate:")?)
}

const CONCLUSION_LIMIT: usize = 4000;

/// Text cut to the 4000 characters a conclusion or a whole reply keeps.
pub fn truncate(text: &str) -> String {
    text.chars().take(CONCLUSION_LIMIT).collect()
}

/// The conclusion an evaluator reply ends with: the reply's last line that
/// begins `DONE:`, `CONTINUE:` or `BLOCKED:`, to the end.
pub fn reply_conclusion(reply: &str) -> Option<(ConclusionKind, String)> {
    let lines: Vec<&str> = reply.split('\n').collect();
    for index in (0..lines.len()).rev() {
        for kind in [
            ConclusionKind::Done,
            ConclusionKind::Continue,
            ConclusionKind::Blocked,
        ] {
            if lines[index].starts_with(kind.prefix()) {
                return Some((kind, truncate(&lines[index..].join("\n"))));
            }
        }
    }
    None
}

/// The conclusion a report block carries (§5). A report is a persisted
/// cross-plugin input, so every attribute it needs is checked here.
pub fn conclusion_of(text: &str) -> Option<Conclusion> {
    let attrs = text::marker(text).unwrap_or_default();
    let result = ConclusionKind::of(attrs.get("result").map(String::as_str)?)?;
    let evaluation = sequence_of(attrs.get("evaluation")?)?;
    let evaluator = attrs.get("evaluator").filter(|value| !value.is_empty())?;
    if attrs.get("kind").map(String::as_str) != Some("report") {
        return None;
    }
    Some(Conclusion {
        evaluation,
        evaluator: evaluator.clone(),
        result,
        text: text::body(text, "Evaluator report:"),
    })
}

/// Whether `event` opens a branch's own history: its `branch/start`, or the
/// `session/forked` a fork writes after the facts it copied.
pub fn is_opening(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::BranchStart(_) | EventKind::SessionForked(_)
    )
}

/// The change trigger the input `data` carries, if any: a `kind="goal"` marker
/// with no `after` (one Goal wrote while deciding), no `result` (a conclusion)
/// and no old key (a change written before the state record existed). The
/// objective or summary the change asks for is decoded from its body.
pub(crate) fn change_of(blocks: &[InputBlock]) -> Option<(Marker, &'static str)> {
    for block in blocks {
        let InputBlock::Raw {
            text: raw,
            key,
            retire: false,
        } = block
        else {
            continue;
        };
        if key.is_some() {
            continue;
        }
        let Some(mut attrs) = text::marker(raw) else {
            continue;
        };
        if attrs.get("kind").map(String::as_str) != Some("goal")
            || attrs.contains_key("after")
            || attrs.contains_key("result")
        {
            continue;
        }
        let op = match attrs.get("op").map(String::as_str) {
            Some("set") => "set",
            Some("edit") => "edit",
            Some("pause") => "pause",
            Some("resume") => "resume",
            Some("clear") => "clear",
            Some("completed") => "completed",
            Some("blocked") => "blocked",
            _ => continue,
        };
        for (label, name) in [
            ("Objective:", "objective"),
            ("Result:", "summary"),
            ("Blocker:", "summary"),
        ] {
            if let Some(value) = text::value(raw, label) {
                attrs.insert(name.to_owned(), value);
            }
        }
        return Some((attrs, op));
    }
    None
}

/// Applies the change `op` with `attrs` to the goal state; one implementation
/// serves the fold's stepping and the record composed with a change before it
/// is folded (R5). Returns whether the change is a request instead of a state
/// change: a `completed` under the evaluator judge leaves the state as it is
/// (A16).
fn apply_change(
    op: &str,
    attrs: &Marker,
    objective: &mut Option<String>,
    state: &mut text::State,
    judge: &mut Judge,
) -> bool {
    match op {
        "set" => {
            *objective = attrs
                .get("objective")
                .filter(|objective| !objective.is_empty())
                .cloned();
            *judge = Judge::of(attrs);
            *state = text::State::Active;
        }
        "edit" => {
            if let Some(changed) = attrs
                .get("objective")
                .filter(|objective| !objective.is_empty())
                .cloned()
            {
                *objective = Some(changed);
            }
        }
        "pause" => {
            *state = text::State::Paused(
                attrs
                    .get("reason")
                    .filter(|reason| !reason.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "user".to_owned()),
            );
        }
        "resume" => {
            *state = text::State::Active;
        }
        "clear" => {
            *objective = None;
            *state = text::State::None;
            *judge = Judge::Model;
        }
        "completed" if *judge == Judge::Evaluator => return true,
        "completed" | "blocked" => {
            let summary = attrs.get("summary").cloned().unwrap_or_default();
            *state = if op == "completed" {
                text::State::Completed(summary)
            } else {
                text::State::Blocked(summary)
            };
        }
        _ => {}
    }
    false
}

#[derive(Clone)]
pub(crate) struct ConclusionRecord {
    sequence: u64,
    answered: bool,
}

/// One branch's goal state: the fold of its consumed local events (§2).
#[derive(Clone, Default)]

pub struct Fold {
    /// The objective the newest Goal block states.
    pub objective: Option<String>,
    /// The status it states; `None` while the objective has none.
    pub state: text::State,
    /// Sequence of the latest change that set `active` (`set` or `resume`);
    /// zero when none did, which every sequence exceeds.
    pub active: u64,
    pub last_operator: u64,
    pub turn_start: u64,

    pub turn_end: Option<TurnEnd>,
    /// How many local goal blocks the branch has: any means it has its own
    /// goal history.
    pub local_blocks: usize,
    /// At a `session/forked` opening: which Goal keys the copied facts left
    /// live.
    pub copied_live: Option<text::Live>,
    /// The newest block of each Goal key on the branch's own segment, which a
    /// `session/forked` opening turns into `copied_live`. A state record
    /// retires the old keys this still holds live (R1).
    pub(crate) live: text::Live,
    /// The judge of the latest `set`, kept by `edit` (Pursuit §3).
    pub judge: Judge,
    /// Under the evaluator judge, the worker's `completed` blocks: a request,
    /// not a status change (A16).
    pub requests: Vec<Request>,
    /// Evaluations created on this branch: `N` → the evaluator branch.
    pub evaluations: BTreeMap<u64, String>,
    /// The first conclusion input from each evaluation's own evaluator.
    pub conclusions: BTreeMap<u64, ConclusionRecord>,
    /// The `continue` conclusions no input for their evaluation has answered
    /// yet: evaluation → (input sequence, conclusion). A continuation or pause
    /// for the evaluation, a newer turn end or an operator input drops the
    /// entry (Pursuit §5).
    pending_conclusions: BTreeMap<u64, (u64, Conclusion)>,
    /// Sequence of the latest Goal block that is not a continuation.
    pub last_block: u64,
    /// The `after` the latest state record names: the change trigger it
    /// answered. A change trigger at or below it was answered (D3).
    pub state_after: u64,
    /// Sequence of the latest change trigger.
    pub last_change: u64,
    /// The lines this branch opened that ended.
    pub ended: HashSet<String>,
    /// The lines this branch could not create.
    pub failed: HashSet<String>,
    /// Turn ends in this branch's local events.
    pub turn_ends: usize,
    /// Background tasks this branch started and has not ended, by `task_id`.
    pub(crate) open_tasks: HashSet<String>,
    pub(crate) last_interrupt: u64,
    continuation_count: usize,
    continuation_idle: usize,
    /// The ids of the continuation inputs, which drive turns (C7).
    pub(crate) continuation_inputs: HashSet<String>,
    /// Whether the latest `turn/start` was driven by Goal's continuation; the
    /// `turn/end` that closes it keeps the answer.
    pub(crate) turn_continuation_driven: bool,
    pub(crate) decided: BTreeSet<String>,

    pub(crate) driving: HashSet<String>,

    pub(crate) record_after: u64,
    /// A state record that names its trigger by submission id (R5): it states
    /// its change when the very next event is that trigger, and states
    /// nothing when any other event comes first.
    held_record: Option<HeldRecord>,
}

/// A `goal:state` record with `trigger="<submission id>"`: the statement it
/// carries waits for the trigger input it names, the next event (R5).
#[derive(Clone)]
struct HeldRecord {
    sequence: u64,
    trigger: String,
    attrs: Marker,
    raw: String,
    retire: bool,
}

pub(crate) enum Source<'a> {
    Opening {
        forked: bool,
    },
    Input {
        id: &'a str,
        submission: Option<&'a str>,
        driving: bool,
        operator: bool,
        blocks: &'a [InputBlock],
    },
    Evaluation {
        evaluation: u64,
        branch: &'a str,
    },
    BranchEnded {
        branch: &'a str,
        failed: bool,
    },
    Consumed {
        input_id: &'a str,
    },
    TurnStart,
    TurnEnd {
        turn_id: &'a str,
        status: TurnStatus,
    },
    Interrupt {
        turn: bool,
    },
    Task {
        task_id: &'a str,
        started: bool,
    },
    Ignored,
}

impl<'a> Source<'a> {
    fn of(event: &'a AgentEvent) -> Self {
        match &event.kind {
            EventKind::BranchStart(_) => Self::Opening { forked: false },
            EventKind::SessionForked(_) => Self::Opening { forked: true },
            EventKind::InputMessage(data) => Self::Input {
                id: &event.id,
                submission: data.submission_id.as_deref(),
                driving: data.effective_mode() != InputMode::Append,
                operator: data.user.is_some()
                    && !data
                        .blocks
                        .first()
                        .is_some_and(|block| block_text(block).starts_with("<dim-")),
                blocks: &data.blocks,
            },
            EventKind::BranchCreate(data) => match &data.source {
                BranchSource::Host { operation_id } => evaluation_of_operation(operation_id)
                    .map_or(Self::Ignored, |evaluation| Self::Evaluation {
                        evaluation,
                        branch: &data.branch,
                    }),
                _ => Self::Ignored,
            },
            EventKind::BranchEnded(data) => Self::BranchEnded {
                branch: &data.branch,
                failed: false,
            },
            EventKind::BranchCreateFailed(data) => Self::BranchEnded {
                branch: match data {
                    dim_agent_types::BranchCreateFailedData::Failed { branch, .. }
                    | dim_agent_types::BranchCreateFailedData::Cancelled { branch, .. } => branch,
                },
                failed: true,
            },
            EventKind::InputConsumed(data) => Self::Consumed {
                input_id: &data.input_id,
            },
            EventKind::InputDeleted(data) => Self::Consumed {
                input_id: &data.input_id,
            },
            EventKind::TurnStart(_) => Self::TurnStart,
            EventKind::TurnEnd(data) => Self::TurnEnd {
                turn_id: &data.turn_id,
                status: TurnStatus::of(&data.outcome),
            },
            EventKind::StopInterrupt(data) => Self::Interrupt {
                turn: data.scope == InterruptScope::Turn,
            },
            EventKind::ExecStarted(data) => Self::Task {
                task_id: &data.task_id,
                started: true,
            },
            EventKind::ExecEnded(data) => Self::Task {
                task_id: &data.task_id,
                started: false,
            },
            _ => Self::Ignored,
        }
    }
}

impl Fold {
    /// Applies one delivered event to this execution.
    pub fn apply(&mut self, event: &AgentEvent) {
        self.apply_source(event.sequence, Source::of(event));
    }

    pub(crate) fn apply_source(&mut self, sequence: u64, source: Source<'_>) {
        if !matches!(source, Source::Input { .. }) {
            self.held_record = None;
        }
        match source {
            Source::Opening { forked } => {
                let copied_live = forked.then_some(self.live);
                *self = Self::default();
                self.copied_live = copied_live;
            }
            Source::Input {
                id,
                submission,
                driving,
                operator,
                blocks,
            } => {
                if driving {
                    self.driving.insert(id.to_owned());
                }
                if operator {
                    self.last_operator = sequence;
                    self.continuation_count = 0;
                    self.continuation_idle = 0;
                    self.pending_conclusions
                        .retain(|evaluation, _| *evaluation > sequence);
                }
                self.goal_input(sequence, id, submission, blocks);
            }
            Source::Evaluation { evaluation, branch } => {
                self.evaluations.insert(evaluation, branch.to_owned());
                self.decided.insert(evaluation.to_string());
            }
            Source::BranchEnded { branch, failed } => {
                if failed {
                    self.failed.insert(branch.to_owned());
                } else {
                    self.ended.insert(branch.to_owned());
                }
            }
            Source::Consumed { input_id } => {
                self.driving.remove(input_id);
                self.continuation_inputs.remove(input_id);
            }
            Source::TurnStart => {
                self.turn_start = sequence;
                self.turn_continuation_driven = self
                    .continuation_inputs
                    .iter()
                    .any(|id| self.driving.contains(id));
            }
            Source::TurnEnd { turn_id, status } => {
                self.turn_end = Some(TurnEnd {
                    sequence,
                    status,
                    turn_id: turn_id.to_owned(),
                    turn_start: self.turn_start,
                    continuation_driven: self.turn_continuation_driven,
                });
                self.turn_ends += 1;
                self.pending_conclusions
                    .retain(|evaluation, _| *evaluation >= sequence);
            }
            Source::Interrupt { turn: true } => {
                self.last_interrupt = sequence;
            }
            Source::Task { task_id, started } => {
                if started {
                    self.open_tasks.insert(task_id.to_owned());
                } else {
                    self.open_tasks.remove(task_id);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn goal_input(
        &mut self,
        sequence: u64,
        id: &str,
        submission: Option<&str>,
        blocks: &[InputBlock],
    ) {
        let held = self.held_record.take();
        self.step_input(sequence, id, blocks);
        if let Some(held) = held {
            self.pair(held, sequence, submission);
        }
        self.retain_requests();
    }

    fn pair(&mut self, held: HeldRecord, sequence: u64, submission: Option<&str>) {
        if held.sequence.checked_add(1) == Some(sequence)
            && submission == Some(held.trigger.as_str())
        {
            self.step_state(
                &held.attrs,
                &held.raw,
                held.retire,
                Some(sequence),
                sequence,
            );
        }
    }

    fn step_input(&mut self, sequence: u64, id: &str, blocks: &[InputBlock]) {
        let first_text = blocks.first().map(block_text);
        let first = first_text.and_then(text::marker);
        // A restatement names the rebuild's cutoff in `after` without
        // answering the fact at it (R6.1): the cutoff can be a conclusion's
        // own sequence, whose deferred decision must still be taken.
        let restated = first
            .as_ref()
            .is_some_and(|attrs| attrs.get("restated").map(String::as_str) == Some("true"));
        if !restated && let Some(after) = first.as_ref().and_then(|attrs| attrs.get("after")) {
            self.decided.insert(after.clone());
            // A continuation or pause for the evaluation answers its
            // conclusion; the deferred one is no longer pending.
            if let Some(number) = sequence_of(after) {
                for (evaluation, conclusion) in &mut self.conclusions {
                    if *evaluation == number || conclusion.sequence == number {
                        conclusion.answered = true;
                    }
                }
                self.pending_conclusions.remove(&number);
                self.pending_conclusions
                    .retain(|_, (input, _)| *input != number);
            }
        }
        if first
            .as_ref()
            .and_then(|attrs| attrs.get("kind"))
            .map(String::as_str)
            == Some("continuation")
        {
            if self.driving.contains(id) {
                self.continuation_inputs.insert(id.to_owned());
            }
            if sequence > self.active.max(self.last_operator) {
                self.continuation_count += 1;
                self.continuation_idle = first
                    .as_ref()
                    .and_then(|attrs| attrs.get("idle"))
                    .and_then(|idle| idle.parse().ok())
                    .unwrap_or(0);
            }
        }
        let conclusion = first_text.and_then(conclusion_of);
        if let Some(conclusion) = &conclusion {
            let evaluation = conclusion.evaluation;
            if self.evaluations.get(&evaluation) == Some(&conclusion.evaluator)
                && !self.conclusions.contains_key(&evaluation)
            {
                self.conclusions.insert(
                    evaluation,
                    ConclusionRecord {
                        sequence,
                        answered: false,
                    },
                );
                if conclusion.result == ConclusionKind::Continue
                    && evaluation > self.last_operator
                    && self
                        .turn_end
                        .as_ref()
                        .is_none_or(|end| evaluation >= end.sequence)
                {
                    self.pending_conclusions
                        .insert(evaluation, (sequence, conclusion.clone()));
                }
            }
        }
        for block in blocks {
            let InputBlock::Raw {
                text: raw,
                key: Some(key),
                retire,
            } = block
            else {
                continue;
            };
            if key != STATE_KEY {
                continue;
            }
            let Some(attrs) = text::marker(raw) else {
                continue;
            };
            self.local_blocks += 1;
            self.step_state(&attrs, raw, *retire, None, sequence);
        }
        if let Some((attrs, op)) = change_of(blocks) {
            self.local_blocks += 1;
            self.last_block = sequence;
            self.last_change = sequence;
            self.step_change(op, &attrs, sequence);
        }
    }

    /// Applies the state one `goal:state` record states (D2). A record whose
    /// `after` is older than the newest change trigger is stale: a later
    /// trigger already asked for another state, and its own record follows.
    /// A record older than the newest record states nothing. A record with
    /// `trigger="<submission id>"` names its change instead of an `after` and
    /// is held until the next event, that trigger, arrives: `paired` carries
    /// the trigger's sequence when it does (R5).
    fn step_state(
        &mut self,
        attrs: &Marker,
        raw: &str,
        retire: bool,
        paired: Option<u64>,
        sequence: u64,
    ) {
        let Some(after) =
            paired.or_else(|| attrs.get("after").and_then(|after| sequence_of(after)))
        else {
            match attrs.get("trigger") {
                Some(trigger) => {
                    self.held_record = Some(HeldRecord {
                        sequence,
                        trigger: trigger.clone(),
                        attrs: attrs.clone(),
                        raw: raw.to_owned(),
                        retire,
                    });
                }
                None => self.live.state = !retire,
            }
            return;
        };
        self.live.state = !retire;
        // A restatement only repairs the published block (R6.1); it answers
        // no fact and yields to any record (R7.1), so it raises no ordering
        // mark: a record taken after the load at a sequence below the
        // rebuild's cutoff still states its state.
        let restated = attrs.get("restated").map(String::as_str) == Some("true");
        if !restated {
            self.record_after = self.record_after.max(after);
        }
        if restated {
            return;
        }
        if after < self.last_change || after < self.state_after {
            return;
        }
        if !restated {
            self.state_after = after;
        }
        if retire {
            self.objective = None;
            self.state = text::State::None;
            self.judge = Judge::Model;
            return;
        }
        let Some(word) = attrs.get("status").map(String::as_str) else {
            return;
        };
        let (objective, summary) = text::state_parts(raw);
        self.objective = objective.filter(|objective| !objective.is_empty());
        self.judge = Judge::of(attrs);
        self.state = match word {
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
            _ => text::State::None,
        };
    }

    /// Applies what a change trigger asked for (D3). The state record that
    /// answers it states the same state; the fold already knows it so a
    /// decision taken between the two reads the change at its sequence (A14).
    fn step_change(&mut self, op: &str, attrs: &Marker, sequence: u64) {
        let request = apply_change(
            op,
            attrs,
            &mut self.objective,
            &mut self.state,
            &mut self.judge,
        );
        if request {
            // A16: under the evaluator judge the worker's `completed` is a
            // request; only the evaluator completes the goal (§4.1).
            self.requests.push(Request {
                sequence,
                summary: attrs.get("summary").cloned().unwrap_or_default(),
            });
            return;
        }
        if matches!(op, "set" | "resume") {
            self.active = sequence;
            self.continuation_count = 0;
            self.continuation_idle = 0;
        }
    }

    pub(crate) fn compact(&mut self, opening: Option<u64>) {
        self.evaluations.retain(|evaluation, branch| {
            !(self.ended.contains(branch) || self.failed.contains(branch))
                || (*evaluation >= self.last_block
                    && !self
                        .conclusions
                        .get(evaluation)
                        .is_some_and(|conclusion| conclusion.answered))
        });
        self.ended
            .retain(|branch| self.evaluations.values().any(|known| known == branch));
        self.failed
            .retain(|branch| self.evaluations.values().any(|known| known == branch));
        self.conclusions
            .retain(|evaluation, _| self.evaluations.contains_key(evaluation));
        self.pending_conclusions
            .retain(|evaluation, _| *evaluation >= self.last_block);
        self.retain_requests();
        let decided = std::mem::take(&mut self.decided);
        self.decided = decided
            .into_iter()
            .filter(|key| {
                sequence_of(key).is_some_and(|sequence| self.needs_decision(sequence, opening))
            })
            .collect();
    }

    fn retain_requests(&mut self) {
        let turn_open = self.turn_open();
        let end = self.turn_end.as_ref();
        let ended_request = self.requests.iter().rposition(|request| {
            end.is_some_and(|end| {
                request.sequence > end.turn_start && request.sequence < end.sequence
            })
        });
        let current_request = self
            .requests
            .iter()
            .rposition(|request| turn_open && request.sequence > self.turn_start);
        let mut index = 0;
        self.requests.retain(|_| {
            let keep = Some(index) == ended_request || Some(index) == current_request;
            index += 1;
            keep
        });
    }

    pub(crate) fn needs_decision(&self, sequence: u64, opening: Option<u64>) -> bool {
        Some(sequence) == opening
            || sequence == self.last_interrupt
            || sequence == self.last_change
            || self
                .turn_end
                .as_ref()
                .is_some_and(|end| sequence == end.sequence)
            || self.evaluations.contains_key(&sequence)
            || self
                .conclusions
                .values()
                .any(|input| input.sequence == sequence)
    }

    /// The goal state this fold states after `op` with `attrs`, as the change
    /// trigger would leave it: the record that commits with the trigger states
    /// it before the trigger is folded (R5).
    pub fn after_change(&self, op: &str, attrs: &Marker) -> (Option<String>, text::State, Judge) {
        let mut objective = self.objective.clone();
        let mut state = self.state.clone();
        let mut judge = self.judge;
        apply_change(op, attrs, &mut objective, &mut state, &mut judge);
        (objective, state, judge)
    }

    /// A goal exists and is active (C1).
    pub fn running(&self) -> bool {
        self.objective.is_some() && self.state == text::State::Active
    }
    pub fn interrupt_after(&self, sequence: u64) -> Option<u64> {
        (self.last_interrupt > sequence).then_some(self.last_interrupt)
    }
    pub fn interrupted(&self) -> bool {
        self.last_interrupt > self.active
    }
    pub fn count(&self) -> usize {
        self.continuation_count
    }
    fn idle(&self) -> usize {
        self.continuation_idle
    }
    pub fn pending_driving(&self) -> bool {
        !self.driving.is_empty()
    }
    /// Whether a background task this branch started is still open (A.4).
    pub fn open_tasks(&self) -> bool {
        !self.open_tasks.is_empty()
    }
    /// The `continue` conclusion still waiting for the branch to settle, with
    /// its input sequence. Reload discards the old execution's wait.
    pub fn pending_conclusion(&self) -> Option<(u64, &Conclusion)> {
        let (evaluation, (sequence, conclusion)) = self.pending_conclusions.last_key_value()?;
        (self
            .conclusions
            .get(evaluation)
            .is_some_and(|record| record.sequence == *sequence)
            && self.is_current(*evaluation))
        .then_some((*sequence, conclusion))
    }
    pub fn turn_open(&self) -> bool {
        self.turn_start > self.turn_end.as_ref().map_or(0, |end| end.sequence)
    }
    /// The worker's completion request inside the turn that ends at `sequence`.
    pub fn request_in_turn(&self, sequence: u64) -> Option<&Request> {
        let start = self
            .turn_end
            .as_ref()
            .filter(|end| end.sequence == sequence)
            .map_or(self.turn_start, |end| end.turn_start);
        self.requests
            .iter()
            .rev()
            .find(|request| request.sequence > start && request.sequence < sequence)
    }
    /// The evaluation the branch `evaluator` was created for, if it is one.
    pub fn evaluation_of(&self, evaluator: &str) -> Option<u64> {
        self.evaluations
            .iter()
            .find(|(_, branch)| *branch == evaluator)
            .map(|(evaluation, _)| *evaluation)
    }
    /// Whether evaluation `N` is still current: no Goal block follows it.
    pub fn is_current(&self, evaluation: u64) -> bool {
        self.objective.is_some() && self.last_block <= evaluation
    }
    pub fn is_decided(&self, sequence: u64) -> bool {
        self.decided.contains(&sequence.to_string())
    }
    /// The newest change trigger Goal has not stated yet (D3, D4). The record
    /// that answered it names it in `after`, so a record naming an older
    /// trigger does not answer a newer one.
    pub fn pending_change(&self) -> Option<u64> {
        (self.last_change > self.state_after).then_some(self.last_change)
    }
}

fn block_text(block: &InputBlock) -> &str {
    let InputBlock::Raw { text, .. } = block;
    text
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Continuation { count: usize, idle: usize },
    Pause { reason: &'static str },
}

/// C6 and C7 over a continuation C1–C5 allow (A.4). `progress` is the tool-call
/// answer for the turn the decision follows; it is needed only when a
/// continuation drove that turn.
fn decide_continuation(
    fold: &Fold,
    limit: Option<u64>,
    end: Option<&TurnEnd>,
    progress: Option<Progress>,
) -> Option<Decision> {
    if limit.is_some_and(|limit| fold.count() as u64 >= limit) {
        return Some(Decision::Pause { reason: "limit" });
    }
    let idle = match end {
        Some(end) if !end.continuation_driven => 0,
        Some(_) => match progress? {
            Progress::ToolCall => 0,
            Progress::NoToolCall => fold.idle() + 1,
        },
        None => 0,
    };
    if idle >= NO_PROGRESS_TURNS {
        return Some(Decision::Pause {
            reason: "no_progress",
        });
    }
    Some(Decision::Continuation {
        count: fold.count() + 1,
        idle,
    })
}

/// The decision for the latest `turn/end` (A.4), over the fold as consumed.
/// `limit` is the document's explicit `maxContinuations`; `None` is no limit.
pub fn decide_turn_end(
    fold: &Fold,
    limit: Option<u64>,
    progress: Option<Progress>,
) -> Option<Decision> {
    let end = fold.turn_end.as_ref()?;
    if fold.is_decided(end.sequence) || !fold.running() {
        return None;
    }
    // A turn end with a background task still open decides nothing: its wake
    // drives the next turn, or its cancelled end takes this decision later.
    if fold.open_tasks() {
        return None;
    }
    if fold.interrupted() {
        return Some(Decision::Pause {
            reason: "interrupted",
        });
    }
    if end.status == TurnStatus::Failed {
        return Some(Decision::Pause {
            reason: "turn_failed",
        });
    }
    if end.status != TurnStatus::Completed || fold.turn_open() || fold.pending_driving() {
        return None;
    }
    decide_continuation(fold, limit, Some(end), progress)
}

/// The decision for a `stop/interrupt {scope: turn}` at `sequence`.
pub fn decide_interrupt(fold: &Fold, sequence: u64) -> Option<Decision> {
    if fold.is_decided(sequence) || !fold.running() || fold.active >= sequence {
        return None;
    }
    Some(Decision::Pause {
        reason: "interrupted",
    })
}

/// Whether the turn ending at `sequence` creates an evaluator instead of
/// continuing (Pursuit §4.1).
pub fn decide_evaluation(fold: &Fold, sequence: u64) -> bool {
    let Some(end) = fold.turn_end.as_ref() else {
        return false;
    };
    if end.sequence != sequence || fold.is_decided(sequence) || !fold.running() {
        return false;
    }
    if fold.interrupted() || end.status != TurnStatus::Completed || fold.turn_open() {
        return false;
    }
    fold.judge == Judge::Evaluator && fold.request_in_turn(sequence).is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConclusionAction {
    Completed(String),
    Blocked(String),
    Continue,
    Failed,
}

/// The action a conclusion input at `sequence` takes (Pursuit §4.1).
pub fn decide_conclusion(
    fold: &Fold,
    sequence: u64,
    conclusion: &Conclusion,
) -> Option<ConclusionAction> {
    let evaluation = conclusion.evaluation;
    if fold
        .conclusions
        .get(&evaluation)
        .map(|record| record.sequence)
        != Some(sequence)
    {
        return None;
    }
    if !fold.is_current(evaluation) || !fold.running() {
        return None;
    }
    Some(match conclusion.result {
        ConclusionKind::Done => ConclusionAction::Completed(conclusion.result_text()),
        ConclusionKind::Blocked => ConclusionAction::Blocked(conclusion.result_text()),
        ConclusionKind::Continue => ConclusionAction::Continue,
        ConclusionKind::Failed => ConclusionAction::Failed,
    })
}

/// Goal's continuation for a `continue` conclusion: C1, C5, C6 and the
/// background wait. C7 measures the branch's own continuations; the evaluator
/// judged this turn with the whole of it in front of it, so the evaluated
/// path does not read progress and states `idle="0"` (Pursuit §5).
pub fn decide_evaluated_continuation(fold: &Fold, limit: Option<u64>) -> Option<Decision> {
    if !fold.running() || fold.pending_driving() || fold.open_tasks() {
        return None;
    }
    if limit.is_some_and(|limit| fold.count() as u64 >= limit) {
        return Some(Decision::Pause { reason: "limit" });
    }
    Some(Decision::Continuation {
        count: fold.count() + 1,
        idle: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::{Conclusion, Fold, conclusion_of};
    use crate::text;
    use dim_agent_types::InputBlock;

    fn report(result: &str, body: &str) -> Conclusion {
        let input = text::conclusion("main", "evaluator", "1", result, body);
        let [InputBlock::Raw { text, .. }] = &input.blocks[..] else {
            panic!("a report is one raw block");
        };
        conclusion_of(text).expect("a report parses")
    }

    #[test]
    fn an_empty_report_body_states_no_result() {
        assert_eq!(report("done", "").result_text(), "");
    }

    #[test]
    fn a_report_without_a_body_line_states_no_result() {
        let text = "<dim-goal kind=\"report\" result=\"done\" evaluation=\"1\" \
                    evaluator=\"evaluator\" branch=\"main\">\nEvaluator report:\n</dim-goal>";
        let conclusion = conclusion_of(text).expect("a report parses");
        assert_eq!(conclusion.result_text(), "");
    }

    #[test]
    fn a_report_body_shorter_than_the_prefix_states_no_result() {
        assert_eq!(report("done", "ok").result_text(), "");
        assert_eq!(report("blocked", "short").result_text(), "");
    }

    #[test]
    fn a_report_body_naming_another_kind_keeps_the_attribute_prefix() {
        assert_eq!(
            report("done", "BLOCKED: gated by policy").result_text(),
            "ED: gated by policy"
        );
    }

    #[test]
    fn a_non_ascii_report_body_slices_utf16_code_units() {
        assert_eq!(report("done", "已完成并验证").result_text(), "证");
        assert_eq!(report("done", "😀abcd").result_text(), "d");
        assert_eq!(report("blocked", "完成").result_text(), "");
    }

    /// R1: each free-text field of a state record sits in its own element with
    /// escaped content, so an objective and a result carrying a heading, a
    /// closing tag, an escaped `&lt;` and a blank line read back byte for byte.
    #[test]
    fn a_state_record_round_trips_its_fields_byte_for_byte() {
        let objective = "Keep the heading.\n\nResult:\nInclude details.\n\nBlocker:\n</objective>\n&lt;\n\nafter.";
        let summary = "Done.\n\nResult:\nall good\n\nBlocker:\nblocked later\n</objective>\n&lt;\n";
        let live = text::Live::default();
        let input = text::state(
            "main",
            Some(objective),
            text::State::Completed(summary.to_owned()),
            text::Judge::Model,
            text::Answered::Sequence(41),
            live,
            &text::GoalTexts::default(),
        );
        let [InputBlock::Raw { text: raw, key, .. }] = &input.blocks[..] else {
            panic!("a record is one raw block while no old key is live");
        };
        assert_eq!(key.as_deref(), Some(text::STATE_KEY));
        let (read_objective, read_summary) = text::state_parts(raw);
        assert_eq!(read_objective.as_deref(), Some(objective));
        assert_eq!(read_summary.as_deref(), Some(summary));

        let event = dim_agent_types::AgentEvent {
            id: "record".into(),
            sequence: 41,
            create_at: 0,
            tags: None,
            parent_call_id: None,
            kind: dim_agent_types::EventKind::InputMessage(input),
        };
        let mut fold = Fold::default();
        fold.apply(&event);
        assert_eq!(fold.objective.as_deref(), Some(objective));
        assert_eq!(fold.state, text::State::Completed(summary.to_owned()));
    }
}
