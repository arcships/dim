//! Goal's language-neutral fold and decisions (design §2, appendix A.4, B).
//!
//! The fold is the state the newest Goal block states, over the branch's own
//! consumed events, from its opening on: a `goal:state` record, or the two old
//! keys while a branch still has only the history written before the record
//! existed. The decisions read only that state.

use crate::text::{self, Judge, Marker, OBJECTIVE_KEY, STATE_KEY, STATUS_KEY};
use dim_agent_types::{
    AgentEvent, BranchSource, EventKind, InputBlock, InputMessageData, InputMode, InterruptScope,
    TurnOutcome,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

/// One continuation input Goal wrote, and the streak it states (C7).
struct Continuation {
    sequence: u64,
    idle: usize,
}

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
pub(crate) fn change_of(data: &InputMessageData) -> Option<(Marker, &'static str)> {
    for block in &data.blocks {
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

/// The state a live old `goal:status` block sets. An operation no status
/// change defines sets a state without one, which no longer counts as active.
fn legacy_state(attrs: &Marker, text: &str) -> text::State {
    if attrs.get("kind").map(String::as_str) == Some("status") {
        return text::State::Active;
    }
    match attrs.get("op").map(String::as_str) {
        Some("pause") => text::State::Paused(
            attrs
                .get("reason")
                .filter(|reason| !reason.is_empty())
                .cloned()
                .unwrap_or_else(|| "user".to_owned()),
        ),
        Some("resume") => text::State::Active,
        Some("completed") => {
            text::State::Completed(text::value(text, "Result:").unwrap_or_default())
        }
        Some("blocked") => text::State::Blocked(text::value(text, "Blocker:").unwrap_or_default()),
        _ => text::State::None,
    }
}

/// One branch's goal state: the fold of its consumed local events (§2).
#[derive(Default)]
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
    pub conclusions: BTreeMap<u64, u64>,
    /// The `continue` conclusions no input for their evaluation has answered
    /// yet: evaluation → (input sequence, conclusion). A continuation or pause
    /// for the evaluation, a newer turn end or an operator input drops the
    /// entry. Settling a deferred turn end re-derives it here, so a reload
    /// keeps it (Pursuit §5).
    pending_conclusions: BTreeMap<u64, (u64, Conclusion)>,
    /// Sequence of the latest Goal block that is not a continuation.
    pub last_block: u64,
    /// The `after` the latest state record names: the change trigger it
    /// answered. A change trigger at or below it was answered (D3).
    pub state_after: u64,
    /// Sequence of the latest change trigger, and the trigger inputs by id.
    pub last_change: u64,
    change_triggers: BTreeMap<String, u64>,
    /// The lines this branch opened that ended.
    pub ended: HashSet<String>,
    /// The lines this branch could not create.
    pub failed: HashSet<String>,
    /// Turn ends in this branch's local events.
    pub turn_ends: usize,
    /// Background tasks this branch started and has not ended, by `task_id`.
    open_tasks: HashSet<String>,
    interrupts: Vec<u64>,
    continuations: Vec<Continuation>,
    /// The ids of the continuation inputs, which drive turns (C7).
    continuation_inputs: HashSet<String>,
    /// Whether the latest `turn/start` was driven by Goal's continuation; the
    /// `turn/end` that closes it keeps the answer.
    turn_continuation_driven: bool,
    decided: BTreeSet<String>,
    driving: HashMap<String, u64>,
    events: Vec<AgentEvent>,
    deleted: HashSet<String>,
    /// The sequences of the inputs `input/deleted` removed. A record whose
    /// `after` names one of them states a change that never took effect and is
    /// stepped as no statement at all (R2).
    deleted_sequences: HashSet<u64>,
    /// The highest `after` among the state records this fold has stepped,
    /// deleted changes included; `0` before the first (sequences start at
    /// one). A deletion asks whether a record naming the withdrawn change, or
    /// a later record, exists (R2), which only the highest can answer.
    record_after: u64,
    /// A state record that names its trigger by submission id (R5): it states
    /// its change when the very next event is that trigger, and states
    /// nothing when any other event comes first.
    held_record: Option<HeldRecord>,
}

/// A `goal:state` record with `trigger="<submission id>"`: the statement it
/// carries waits for the trigger input it names, the next event (R5).
struct HeldRecord {
    trigger: String,
    attrs: Marker,
    raw: String,
    retire: bool,
}

impl Fold {
    /// Applies one consumed local event; `input/deleted` refolds without it.
    pub fn apply(&mut self, event: &AgentEvent) {
        if let EventKind::InputDeleted(data) = &event.kind {
            self.deleted.insert(data.input_id.clone());
            self.events.push(event.clone());
            self.refold();
            return;
        }
        self.events.push(event.clone());
        self.step(event);
    }

    fn refold(&mut self) {
        let events = std::mem::take(&mut self.events);
        let copied_live = self.copied_live;
        *self = Self::default();
        self.copied_live = copied_live;
        self.deleted = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::InputDeleted(data) => Some(data.input_id.clone()),
                _ => None,
            })
            .collect();
        for event in &events {
            self.step(event);
        }
        self.events = events;
    }

    /// Starts the fold of a branch whose own segment has no goal (D4): only the
    /// keys a fork's copied prefix left live and, when the newest Goal block is
    /// a retired state record, that record, which makes the branch's own
    /// history and answers its opening.
    pub fn start_without_goal(&mut self, copied_live: Option<text::Live>, newest: Option<u64>) {
        *self = Self::default();
        self.copied_live = copied_live;
        if let Some(sequence) = newest {
            self.local_blocks = 1;
            self.last_block = sequence;
        }
    }

    /// Restarts the fold at a branch's own opening, keeping the ids of the
    /// deleted inputs and, for a fork, which keys the copied facts left live.
    fn open(&mut self, event: &AgentEvent) {
        let copied_live = matches!(event.kind, EventKind::SessionForked(_)).then_some(self.live);
        let events = std::mem::take(&mut self.events);
        let deleted = std::mem::take(&mut self.deleted);
        *self = Self::default();
        self.events = events;
        self.deleted = deleted;
        self.copied_live = copied_live;
    }

    fn step(&mut self, event: &AgentEvent) {
        let sequence = event.sequence;
        // A held record names its trigger, the next event (R5): only that
        // input pairs with it; any other event lets it state nothing.
        let held = match &event.kind {
            EventKind::InputMessage(_) => self.held_record.take(),
            _ => {
                self.held_record = None;
                None
            }
        };
        if is_opening(&event.kind) {
            self.open(event);
            return;
        }
        match &event.kind {
            EventKind::InputMessage(data) => {
                self.step_input(event, data, sequence);
                if let Some(held) = held
                    && !self.deleted.contains(&event.id)
                    && data.submission_id.as_deref() == Some(held.trigger.as_str())
                {
                    // The trigger's change is applied above; the record states
                    // it at the trigger's sequence (A14).
                    self.step_state(&held.attrs, &held.raw, held.retire, Some(sequence));
                }
            }
            EventKind::BranchCreate(data) => {
                let BranchSource::Host { operation_id } = &data.source else {
                    return;
                };
                let Some(evaluation) = evaluation_of_operation(operation_id) else {
                    return;
                };
                self.evaluations.insert(evaluation, data.branch.clone());
                self.decided.insert(evaluation.to_string());
            }
            EventKind::BranchEnded(data) => {
                self.ended.insert(data.branch.clone());
            }
            EventKind::BranchCreateFailed(data) => {
                let branch = match data {
                    dim_agent_types::BranchCreateFailedData::Failed { branch, .. } => branch,
                    dim_agent_types::BranchCreateFailedData::Cancelled { branch, .. } => branch,
                };
                self.failed.insert(branch.clone());
            }
            EventKind::InputConsumed(data) => {
                self.driving.remove(&data.input_id);
            }
            EventKind::TurnStart(_) => {
                self.turn_start = sequence;
                self.turn_continuation_driven = self
                    .driving
                    .keys()
                    .any(|id| self.continuation_inputs.contains(id));
            }
            EventKind::TurnEnd(data) => {
                self.turn_end = Some(TurnEnd {
                    sequence,
                    status: TurnStatus::of(&data.outcome),
                    turn_id: data.turn_id.clone(),
                    turn_start: self.turn_start,
                    continuation_driven: self.turn_continuation_driven,
                });
                self.turn_ends += 1;
                // A newer turn end supersedes a conclusion still waiting on a
                // background task: the newer end decides from its own state.
                self.pending_conclusions
                    .retain(|evaluation, _| *evaluation >= sequence);
            }
            EventKind::StopInterrupt(data) if data.scope == InterruptScope::Turn => {
                self.interrupts.push(sequence);
            }
            EventKind::ExecStarted(data) => {
                self.open_tasks.insert(data.task_id.clone());
            }
            EventKind::ExecEnded(data) => {
                self.open_tasks.remove(&data.task_id);
            }
            _ => {}
        }
    }

    fn step_input(&mut self, event: &AgentEvent, data: &InputMessageData, sequence: u64) {
        if self.deleted.contains(&event.id) {
            // The input is gone; a record naming it states a change that never
            // took effect (R2).
            self.deleted_sequences.insert(sequence);
            return;
        }
        if data.effective_mode() != InputMode::Append {
            self.driving.insert(event.id.clone(), sequence);
        }
        let first_text = data.blocks.first().map(block_text);
        let first = first_text.and_then(text::marker);
        if data.user.is_some() && !first_text.is_some_and(|text| text.starts_with("<dim-")) {
            self.last_operator = sequence;
            // An operator input supersedes a conclusion still waiting on a
            // background task: the operator's input decides what comes next.
            self.pending_conclusions
                .retain(|evaluation, _| *evaluation > sequence);
        }
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
            self.continuation_inputs.insert(event.id.clone());
            self.continuations.push(Continuation {
                sequence,
                idle: first
                    .as_ref()
                    .and_then(|attrs| attrs.get("idle"))
                    .and_then(|idle| idle.parse().ok())
                    .unwrap_or(0),
            });
        }
        let conclusion = first_text.and_then(conclusion_of);
        if let Some(conclusion) = &conclusion {
            let evaluation = conclusion.evaluation;
            if self.evaluations.get(&evaluation) == Some(&conclusion.evaluator)
                && !self.conclusions.contains_key(&evaluation)
            {
                self.conclusions.insert(evaluation, sequence);
                if conclusion.result == ConclusionKind::Continue {
                    self.pending_conclusions
                        .insert(evaluation, (sequence, conclusion.clone()));
                }
            }
        }
        // An input that carries a state record states the state; the old-key
        // retires that ride with it (R1) are part of that statement, not a
        // change of their own.
        let stated = data.blocks.iter().any(
            |block| matches!(block, InputBlock::Raw { key: Some(key), .. } if key == STATE_KEY),
        );
        for raw in &data.blocks {
            let InputBlock::Raw {
                text: raw_text,
                key,
                retire,
            } = raw;
            let Some(attrs) = text::marker(raw_text) else {
                continue;
            };
            let key = key.as_deref();
            if !matches!(
                key,
                Some(STATE_KEY) | Some(OBJECTIVE_KEY) | Some(STATUS_KEY)
            ) {
                continue;
            }
            self.local_blocks += 1;
            // A state record is Goal's own statement of a change, not a change
            // of its own: a record answering a trigger inside an evaluated turn
            // may commit after the evaluation, and it must not make that
            // evaluation stale. Only the old keys a *change* wrote advance the
            // newest block the evaluation compares to.
            if key != Some(STATE_KEY) && !stated {
                self.last_block = sequence;
            }
            if key == Some(STATE_KEY) {
                self.step_state(&attrs, raw_text, *retire, None);
            } else if stated {
                // The old-key retires that ride with a state record are part
                // of that statement (R1): they end the key, and the state the
                // record states stands.
                match key {
                    Some(OBJECTIVE_KEY) => self.live.objective = !retire,
                    Some(STATUS_KEY) => self.live.status = !retire,
                    _ => {}
                }
            } else {
                self.step_legacy(key.unwrap_or_default(), &attrs, raw_text, *retire, sequence);
            }
        }
        if let Some((attrs, op)) = change_of(data) {
            self.local_blocks += 1;
            self.last_block = sequence;
            self.last_change = sequence;
            self.change_triggers.insert(event.id.clone(), sequence);
            self.step_change(op, &attrs, sequence);
        }
    }

    /// Applies the state one `goal:state` record states (D2). A record whose
    /// `after` is older than the newest change trigger is stale: a later
    /// trigger already asked for another state, and its own record follows.
    /// A record naming a withdrawn change (R2), or one older than the newest
    /// record this fold already stepped (R4), states nothing. A record with
    /// `trigger="<submission id>"` names its change instead of an `after` and
    /// is held until the next event, that trigger, arrives: `paired` carries
    /// the trigger's sequence when it does (R5).
    fn step_state(&mut self, attrs: &Marker, raw: &str, retire: bool, paired: Option<u64>) {
        let Some(after) =
            paired.or_else(|| attrs.get("after").and_then(|after| sequence_of(after)))
        else {
            match attrs.get("trigger") {
                Some(trigger) => {
                    self.held_record = Some(HeldRecord {
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
        if self.deleted_sequences.contains(&after) {
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

    /// Applies a live block under one of the old keys, by the rules the
    /// history written before the state record existed uses (§3, A.3).
    fn step_legacy(&mut self, key: &str, attrs: &Marker, raw: &str, retire: bool, sequence: u64) {
        if key == OBJECTIVE_KEY {
            self.live.objective = !retire;
            if retire {
                self.objective = None;
                self.judge = Judge::Model;
            } else {
                self.objective = text::value(raw, "Objective:");
                if attrs.get("op").map(String::as_str) == Some("set") {
                    self.judge = Judge::of(attrs);
                }
            }
            return;
        }
        self.live.status = !retire;
        if retire {
            self.state = text::State::None;
            return;
        }
        let state = legacy_state(attrs, raw);
        if matches!(state, text::State::Completed(_))
            && attrs.get("by").map(String::as_str) != Some("evaluator")
            && self.judge == Judge::Evaluator
        {
            // A16: under the evaluator judge the worker's `completed` is a
            // request; only the evaluator completes the goal (§4.1).
            self.requests.push(Request {
                sequence,
                summary: state.summary().unwrap_or_default().to_owned(),
            });
            return;
        }
        let active_now = state == text::State::Active
            && (attrs.get("op").map(String::as_str) == Some("resume")
                || attrs.get("kind").map(String::as_str) == Some("status"));
        self.state = state;
        if active_now {
            self.active = sequence;
        }
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
        }
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
    pub fn interrupted(&self) -> bool {
        self.interrupts
            .iter()
            .any(|sequence| *sequence > self.active)
    }
    pub fn count(&self) -> usize {
        let since = self.active.max(self.last_operator);
        self.continuations
            .iter()
            .filter(|continuation| continuation.sequence > since)
            .count()
    }
    /// The streak the latest continuation after the count's cutoff states: the
    /// continuation-driven turns without a tool call ending at the turn it
    /// follows (C7). A continuation from before an operator input states none.
    fn idle(&self) -> usize {
        let since = self.active.max(self.last_operator);
        self.continuations
            .iter()
            .rev()
            .find(|continuation| continuation.sequence > since)
            .map_or(0, |continuation| continuation.idle)
    }
    pub fn pending_driving(&self) -> bool {
        self.driving.keys().any(|id| !self.deleted.contains(id))
    }
    /// Whether a background task this branch started is still open (A.4).
    pub fn open_tasks(&self) -> bool {
        !self.open_tasks.is_empty()
    }
    /// The `continue` conclusion still waiting for the branch to settle, with
    /// its input sequence. It is re-derived from the fold, so a reload keeps
    /// it (Pursuit §5).
    pub fn pending_conclusion(&self) -> Option<(u64, &Conclusion)> {
        let (evaluation, (sequence, conclusion)) = self.pending_conclusions.last_key_value()?;
        (self.conclusions.get(evaluation) == Some(sequence) && self.is_current(*evaluation))
            .then_some((*sequence, conclusion))
    }
    pub fn turn_open(&self) -> bool {
        self.turn_start > self.turn_end.as_ref().map_or(0, |end| end.sequence)
    }
    /// The worker's completion request inside the turn that ends at `sequence`.
    pub fn request_in_turn(&self, sequence: u64) -> Option<&Request> {
        self.requests
            .iter()
            .rev()
            .find(|request| request.sequence > self.turn_start && request.sequence < sequence)
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
        self.last_block <= evaluation
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
    /// The sequence of the change trigger input `id`, when this fold holds it.
    pub fn change_trigger(&self, id: &str) -> Option<u64> {
        self.change_triggers.get(id).copied()
    }
    /// Whether a state record naming `after` or a later change has been
    /// stepped: the newest live record states that state, not the withdrawn
    /// change's (R2).
    pub fn recorded_at_or_after(&self, after: u64) -> bool {
        self.record_after >= after
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
    if fold.conclusions.get(&evaluation).copied() != Some(sequence) {
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
            kind: dim_agent_types::EventKind::InputMessage(input),
        };
        let mut fold = Fold::default();
        fold.apply(&event);
        assert_eq!(fold.objective.as_deref(), Some(objective));
        assert_eq!(fold.state, text::State::Completed(summary.to_owned()));
    }
}
