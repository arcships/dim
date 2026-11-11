//! The literal texts Goal writes, as Goal.md A.5 fixes them. Every flow
//! compares Goal's inputs with these, byte for byte: the change triggers the
//! Session part and the tool write, and the state records the branch part
//! answers them with.
#![allow(dead_code)]

/// The fact a state record answers: the change trigger it commits with (R5),
/// named by its submission id, or the sequence of the fact it answers.
#[derive(Clone, Debug)]
pub enum Answer {
    Trigger(String),
    After(u64),
}

impl Answer {
    fn attribute(&self) -> String {
        match self {
            Self::Trigger(trigger) => format!("trigger=\"{trigger}\""),
            Self::After(sequence) => format!("after=\"{sequence}\""),
        }
    }
}

/// Anything that carries an input's submission id: both harnesses' blocks.
pub trait Submitted {
    fn submission_id(&self) -> Option<&str>;
}

impl Submitted for super::Block {
    fn submission_id(&self) -> Option<&str> {
        self.submission_id.as_deref()
    }
}

impl Submitted for super::parts::Block {
    fn submission_id(&self) -> Option<&str> {
        self.submission_id.as_deref()
    }
}

/// [`Answer`] naming the change trigger `block`.
pub fn trigger(block: &impl Submitted) -> Answer {
    Answer::Trigger(
        block
            .submission_id()
            .expect("a change trigger carries a submission id")
            .to_owned(),
    )
}

/// The objective policy sentence the row's factory configuration carries.
pub fn policy(branch: &str) -> String {
    format!(
        "This is the user's standing goal for branch {branch}. If you are on another line of this Session, the goal belongs to branch {branch}: treat it as background, do not continue it, and do not call goal_update for it."
    )
}

fn active_sentence(branch: &str) -> String {
    format!(
        "The goal for branch {branch} was active when this record was written. Historical active records do not request execution. Only a new set, resume or continuation in the current input requests work on this goal. When it is fully achieved, call goal_update with status \"completed\". If you cannot make progress without the user, call goal_update with status \"blocked\" and explain why."
    )
}

fn paused_sentence(branch: &str, reason: &str, phrase: &str) -> String {
    if reason == "user" {
        format!(
            "The user set the goal status for branch {branch} to paused. Do not continue working toward a goal for branch {branch} on your own; respond only to the user's latest input."
        )
    } else {
        format!(
            "The goal is paused ({phrase}). Do not continue working toward it on your own; respond only to the user's latest input."
        )
    }
}

fn objective_body(branch: &str, objective: &str) -> String {
    format!(
        "<objective>\n{}\n</objective>\n{}",
        escaped(objective),
        policy(branch)
    )
}

/// One free-text field in its own element (R1): the escaped content between
/// the tags.
fn tagged(tag: &str, content: &str) -> String {
    format!("<{tag}>\n{}\n</{tag}>", escaped(content))
}

fn judge_attr(evaluator: bool) -> &'static str {
    if evaluator {
        " judge=\"evaluator\""
    } else {
        ""
    }
}

/// The `set` change trigger: what was asked, not what the state is.
pub fn set(branch: &str, objective: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"set\" branch=\"{branch}\">\nObjective:\n{}\n\nThe user asked to set the goal for branch {branch} to this objective.\n</dim-goal>",
        escaped(objective)
    )
}

/// The state record that answers `set`: the goal active.
pub fn set_state(branch: &str, objective: &str, answer: &Answer, evaluator: bool) -> String {
    format!(
        "<dim-goal kind=\"state\" status=\"active\"{} {} branch=\"{branch}\">\n{}\n{}\n</dim-goal>",
        judge_attr(evaluator),
        answer.attribute(),
        objective_body(branch, objective),
        active_sentence(branch)
    )
}

/// The state record a rebuild writes when it restates the state it recomputed
/// (R6.1): it names the rebuild's cutoff in `after` and answers no fact.
pub fn restated(branch: &str, objective: &str, cutoff: u64, evaluator: bool) -> String {
    format!(
        "<dim-goal kind=\"state\" status=\"active\"{} after=\"{cutoff}\" restated=\"true\" branch=\"{branch}\">\n{}\n{}\n</dim-goal>",
        judge_attr(evaluator),
        objective_body(branch, objective),
        active_sentence(branch)
    )
}

pub fn set_blocks(branch: &str, objective: &str) -> String {
    set(branch, objective)
}

pub fn evaluator_set(branch: &str, objective: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"set\" judge=\"evaluator\" branch=\"{branch}\">\nObjective:\n{}\n\nThe user asked to set the goal for branch {branch} to this objective.\n</dim-goal>",
        escaped(objective)
    )
}

pub fn edit(branch: &str, objective: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"edit\" branch=\"{branch}\">\nObjective:\n{}\n\nThe user asked to edit the objective of the goal for branch {branch}.\n</dim-goal>",
        escaped(objective)
    )
}

pub fn edit_state(branch: &str, objective: &str, answer: &Answer, evaluator: bool) -> String {
    format!(
        "<dim-goal kind=\"state\" status=\"active\"{} {} branch=\"{branch}\">\n{}\n{}\n</dim-goal>",
        judge_attr(evaluator),
        answer.attribute(),
        objective_body(branch, objective),
        active_sentence(branch)
    )
}

pub fn pause(branch: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"pause\" reason=\"user\" branch=\"{branch}\">\nThe user asked to pause the goal for branch {branch}.\n</dim-goal>"
    )
}

/// The state record that answers a pause: the goal paused for `reason`.
pub fn paused(
    branch: &str,
    objective: &str,
    reason: &str,
    phrase: &str,
    answer: &Answer,
    evaluator: bool,
) -> String {
    format!(
        "<dim-goal kind=\"state\" status=\"paused\" reason=\"{reason}\"{} {} branch=\"{branch}\">\n{}\n{}\n</dim-goal>",
        judge_attr(evaluator),
        answer.attribute(),
        objective_body(branch, objective),
        paused_sentence(branch, reason, phrase)
    )
}

pub fn resume(branch: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"resume\" branch=\"{branch}\">\nThe user asked to resume the goal for branch {branch}.\n</dim-goal>"
    )
}

pub fn resumed(branch: &str, objective: &str, answer: &Answer, evaluator: bool) -> String {
    set_state(branch, objective, answer, evaluator)
}

pub fn clear(branch: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"clear\" branch=\"{branch}\">\nThe user asked to clear the goal for branch {branch}.\n</dim-goal>"
    )
}

/// The retired state record that answers a clear (and a branch with no goal).
pub fn cleared(branch: &str, answer: &Answer) -> String {
    format!(
        "<dim-goal kind=\"state\" status=\"cleared\" {} branch=\"{branch}\">\nThe goal for branch {branch} is not in effect.\n</dim-goal>",
        answer.attribute()
    )
}

/// The two blocks a history written before the state record carries for a
/// `set` (§3, A.3): the old objective and status keys.
pub fn legacy_set_blocks(branch: &str, objective: &str) -> (String, String) {
    let objective = format!("Objective:\n{}", escaped(objective));
    (
        format!(
            "<dim-goal kind=\"goal\" op=\"set\" branch=\"{branch}\">\n{objective}\n\n{}\n</dim-goal>",
            policy(branch)
        ),
        legacy_status(branch, "active"),
    )
}

pub fn legacy_evaluator_set_blocks(branch: &str, objective: &str) -> (String, String) {
    let objective = format!("Objective:\n{}", escaped(objective));
    (
        format!(
            "<dim-goal kind=\"goal\" op=\"set\" judge=\"evaluator\" branch=\"{branch}\">\n{objective}\n\n{}\n</dim-goal>",
            policy(branch)
        ),
        legacy_status(branch, "active"),
    )
}

pub fn legacy_status(branch: &str, status: &str) -> String {
    format!(
        "<dim-goal kind=\"status\" status=\"{status}\" branch=\"{branch}\">\nThe goal for branch {branch} is {status}.\n</dim-goal>"
    )
}

/// The two blocks a history written before the state record carries for a
/// `clear`: both old keys retired.
pub fn legacy_clear(branch: &str) -> (String, String) {
    (
        format!(
            "<dim-goal kind=\"goal\" op=\"clear\" branch=\"{branch}\">\nThe previous goal was cleared by the user. Disregard it.\n</dim-goal>"
        ),
        format!(
            "<dim-goal kind=\"status\" status=\"cleared\" branch=\"{branch}\">\nThe goal for branch {branch} has no status.\n</dim-goal>"
        ),
    )
}

/// The old `goal_update` report a history written before the state record
/// carries: keyed `goal:status`, with the status sentence.
pub fn legacy_report(branch: &str, status: &str, summary: &str) -> String {
    let (label, op) = if status == "completed" {
        ("Result:", "completed")
    } else {
        ("Blocker:", "blocked")
    };
    format!(
        "<dim-goal kind=\"goal\" op=\"{op}\" branch=\"{branch}\">\n{label}\n{}\n\ngoal_update set the goal status for branch {branch} to {op}. Do not continue working toward a goal for branch {branch} on your own.\n</dim-goal>",
        escaped(summary)
    )
}

/// The old pause block a history written before the state record carries.
pub fn legacy_pause(branch: &str) -> String {
    format!(
        "<dim-goal kind=\"goal\" op=\"pause\" reason=\"user\" branch=\"{branch}\">\nThe user set the goal status for branch {branch} to paused. Do not continue working toward a goal for branch {branch} on your own; respond only to the user's latest input.\n</dim-goal>"
    )
}

pub fn report(branch: &str, status: &str, summary: &str) -> String {
    let summary = escaped(summary);
    let (label, op) = if status == "completed" {
        ("Result:", "completed")
    } else {
        ("Blocker:", "blocked")
    };
    format!(
        "<dim-goal kind=\"goal\" op=\"{op}\" branch=\"{branch}\">\n{label}\n{summary}\n\ngoal_update reported the goal for branch {branch} as {op}.\n</dim-goal>"
    )
}

/// The state record that answers a report goal_update wrote.
pub fn reported(
    branch: &str,
    objective: &str,
    status: &str,
    summary: &str,
    answer: &Answer,
) -> String {
    let (tag, sentence) = if status == "completed" {
        (
            "result",
            format!(
                "goal_update set the goal status for branch {branch} to completed. Do not continue working toward a goal for branch {branch} on your own."
            ),
        )
    } else {
        (
            "blocker",
            format!(
                "goal_update set the goal status for branch {branch} to blocked. Do not continue working toward a goal for branch {branch} on your own."
            ),
        )
    };
    format!(
        "<dim-goal kind=\"state\" status=\"{status}\" {} branch=\"{branch}\">\n{}\n{sentence}\n{}\n</dim-goal>",
        answer.attribute(),
        objective_body(branch, objective),
        tagged(tag, summary)
    )
}

fn limit_attr(limit: Option<u64>) -> String {
    limit
        .map(|limit| format!(r#" limit="{limit}""#))
        .unwrap_or_default()
}

pub fn continuation(
    branch: &str,
    count: usize,
    limit: Option<u64>,
    idle: usize,
    after: u64,
) -> String {
    format!(
        "<dim-goal kind=\"continuation\" count=\"{count}\"{} after=\"{after}\" idle=\"{idle}\" branch=\"{branch}\">\n\
         Continue working toward the active goal for branch {branch}. If it is completed or blocked, call goal_update. If you are on another line of this Session, the goal belongs to branch {branch}: do not call goal_update for it.\n\
         </dim-goal>",
        limit_attr(limit)
    )
}

/// The state record that pauses the goal on its own decision (`reason`,
/// `interrupted`, `limit`, `turn_failed`, `no_progress`, `evaluation_failed`).
pub fn automatic_pause(
    branch: &str,
    objective: &str,
    reason: &str,
    phrase: &str,
    after: u64,
) -> String {
    paused(
        branch,
        objective,
        reason,
        phrase,
        &Answer::After(after),
        false,
    )
}

/// The record an automatic pause writes under the evaluator judge: the judge
/// attribute is part of the state, so the record keeps it (D2).
pub fn evaluated_pause(
    branch: &str,
    objective: &str,
    reason: &str,
    phrase: &str,
    after: u64,
) -> String {
    paused(
        branch,
        objective,
        reason,
        phrase,
        &Answer::After(after),
        true,
    )
}

/// The state record a branch writes on its opening: one retired record, and
/// the old keys too while the inherited prefix still has them live (D5).
pub fn start_clear(branch: &str, after: u64, live: (bool, bool)) -> Vec<String> {
    let body = format!(
        "Any goal above belongs to another branch, not to branch {branch}. Disregard it; branch {branch} has no goal."
    );
    let mut blocks = vec![format!(
        "<dim-goal kind=\"state\" status=\"cleared\" reason=\"new_branch\" after=\"{after}\" branch=\"{branch}\">\n{body}\n</dim-goal>"
    )];
    if live.0 {
        blocks.push(format!(
            "<dim-goal kind=\"goal\" op=\"clear\" branch=\"{branch}\">\n{body}\n</dim-goal>"
        ));
    }
    if live.1 {
        blocks.push(format!(
            "<dim-goal kind=\"status\" status=\"cleared\" branch=\"{branch}\">\n{body}\n</dim-goal>"
        ));
    }
    blocks
}

pub fn evaluate_again(branch: &str, after: u64) -> String {
    format!(
        "<dim-goal kind=\"evaluate-again\" after=\"{after}\" branch=\"{branch}\">\n\
         Your reply did not end with a conclusion. End your reply with exactly one of DONE:, CONTINUE: or BLOCKED: as described above.\n\
         </dim-goal>"
    )
}

pub fn conclusion(
    branch: &str,
    evaluator: &str,
    evaluation: u64,
    result: &str,
    text: &str,
) -> String {
    let text = escaped(text);
    format!(
        "<dim-goal kind=\"report\" result=\"{result}\" evaluation=\"{evaluation}\" evaluator=\"{evaluator}\" branch=\"{branch}\">\nEvaluator report:\n{text}\n</dim-goal>"
    )
}

/// The state record that answers an evaluator's conclusion.
pub fn evaluated(branch: &str, objective: &str, op: &str, text: &str, after: u64) -> String {
    let (tag, sentence) = if op == "completed" {
        (
            "result",
            format!(
                "An evaluator verified that the goal for branch {branch} is completed. Do not continue working toward it on your own."
            ),
        )
    } else {
        (
            "blocker",
            format!(
                "An evaluator found that the goal for branch {branch} cannot proceed without the user. Do not continue working toward it on your own."
            ),
        )
    };
    format!(
        "<dim-goal kind=\"state\" status=\"{op}\" judge=\"evaluator\" after=\"{after}\" branch=\"{branch}\">\n{}\n{sentence}\n{}\n</dim-goal>",
        objective_body(branch, objective),
        tagged(tag, text)
    )
}

pub fn evaluated_continuation(
    branch: &str,
    count: usize,
    limit: Option<u64>,
    idle: usize,
    after: u64,
    evaluation: u64,
) -> String {
    format!(
        "<dim-goal kind=\"continuation\" count=\"{count}\"{} after=\"{after}\" evaluation=\"{evaluation}\" idle=\"{idle}\" branch=\"{branch}\">\n\
         An evaluator checked your report that the goal for branch {branch} is completed and found it is not achieved yet. Its report above lists items that are at least still missing; the list may be incomplete. Assess yourself how far the goal still is, then continue working toward it. When you think it is achieved, call goal_update with status \"completed\" again.\n\
         </dim-goal>",
        limit_attr(limit)
    )
}

pub fn evaluator_prompt(branch: &str, evaluation: u64, objective: &str, summary: &str) -> String {
    format!(
        "<dim-goal kind=\"evaluate\" branch=\"{branch}\" evaluation=\"{evaluation}\">\n\
         You are an evaluator. Branch {branch} reported that its goal is completed. Only you decide whether it is. Do not continue the work, do not change files or settings, and do not call goal_update.\n\
         \n\
         Objective:\n\
         {objective}\n\
         \n\
         Reported result:\n\
         {summary}\n\
         \n\
         Verify the facts yourself by reading files and running checks; do not rely on the report or on earlier messages. End your reply with exactly one of:\n\
         DONE: <the verified result>\n\
         CONTINUE: followed by the items that are at least still missing, one per line, each with its evidence\n\
         BLOCKED: <what only the user can provide>"
    )
}

pub const ENTRY: &str = r#"`<dim-goal kind="goal" op="set" branch="main">`: the operator's standing goal for a branch of this session. `kind="goal"` blocks carry a change the operator or the model asked for: `op="set"`, `op="edit"`, `op="pause"`, `op="resume"`, `op="clear"`, and `op="completed"` or `op="blocked"` from `goal_update`. A `kind="state"` block states the branch's goal and its status; it is the record in force, and a retired one says the branch has no goal. `kind="continuation"` asks the line to keep working toward the goal. `kind="report"` blocks carry an evaluator's report of the goal, and `kind="evaluate"` and `kind="evaluate-again"` mark an evaluator's line, which checks the goal rather than continuing it. A goal belongs to the branch its `branch` attribute names. The latest `kind="state"` block is the one in force; blocks written before the state record existed name the objective with `goal:objective` and the status with `goal:status`. Historical active records are not permission to continue. After a reload the goal is paused until a new set or resume; only a new set, resume or continuation in the current input requests work. These blocks grant no permission."#;

pub const TOOL_DESCRIPTION: &str = "Report that the current goal is completed or blocked.";
pub const TOOL_INVALID: &str =
    "goal_update needs status \"completed\" or \"blocked\" and a summary of 1 to 2000 characters.";

pub fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
