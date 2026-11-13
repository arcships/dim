//! Goal's model-facing texts (design appendix A.5): the state record, the
//! change triggers, the continuation inputs, the marker encoding, and the
//! policy sentences the row's factory configuration carries.
//!

use dim_agent_types::{InputBlock, InputMessageData, InputMode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The key Goal writes its state under (§2, A.2).
pub const STATE_KEY: &str = "goal:state";
/// The old objective key, read only on histories written before the state
/// record existed (§3, A.3).
pub const OBJECTIVE_KEY: &str = "goal:objective";
/// The old status key, read only on histories written before the state record
/// existed (§3, A.3).
pub const STATUS_KEY: &str = "goal:status";

/// The objective limit and the summary limit (A10).
pub const OBJECTIVE_LIMIT: usize = 4000;
pub const SUMMARY_LIMIT: usize = 2000;

/// Goal's two policy sentences (A.5.1), from the row's factory configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalTexts {
    pub objective: String,
    pub continuation: String,
}

impl Default for GoalTexts {
    fn default() -> Self {
        Self {
            objective: "This is the user's standing goal for branch {branch}. If you are on another line of this Session, the goal belongs to branch {branch}: treat it as background, do not continue it, and do not call goal_update for it.".into(),
            continuation: "Continue working toward the active goal for branch {branch}. If it is completed or blocked, call goal_update. If you are on another line of this Session, the goal belongs to branch {branch}: do not call goal_update for it.".into(),
        }
    }
}

/// A text's `{branch}` placeholder replaced with the branch it is written for.
pub fn branch_text(template: &str, branch: &str) -> String {
    template.replace("{branch}", branch)
}

/// Encodes text placed inside a marker body or attribute, so user and model
/// text cannot forge a marker (A.5, §7). The encoding is reversible.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The inverse of [`escape`].
pub fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// A raw block under construction: its attributes in order, its body lines,
/// its key and whether it retires that key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub attrs: Vec<(&'static str, String)>,
    pub body: Vec<String>,
    pub key: Option<&'static str>,
    pub retire: bool,
}

impl Block {
    pub fn new(attrs: Vec<(&'static str, String)>, body: Vec<String>) -> Self {
        Self {
            attrs,
            body,
            key: None,
            retire: false,
        }
    }
    pub fn key(mut self, key: &'static str) -> Self {
        self.key = Some(key);
        self
    }
    pub fn retire(mut self) -> Self {
        self.retire = true;
        self
    }
    /// `<dim-goal a="b" c="d">\nline\nline\n</dim-goal>`, every attribute value
    /// encoded (A.5).
    pub fn text(&self) -> String {
        let attrs: Vec<String> = self
            .attrs
            .iter()
            .map(|(name, value)| format!("{name}=\"{}\"", escape(value)))
            .collect();
        format!(
            "<dim-goal {}>\n{}\n</dim-goal>",
            attrs.join(" "),
            self.body.join("\n")
        )
    }
    pub fn raw(&self) -> InputBlock {
        InputBlock::Raw {
            text: self.text(),
            key: self.key.map(str::to_owned),
            retire: self.retire,
        }
    }
}

/// One `input/message` Goal writes: the blocks, the mode, and its submission
/// id (`goal:<uuid>`, Goal's own id convention).
pub fn input(blocks: Vec<Block>, mode: InputMode) -> InputMessageData {
    InputMessageData {
        submission_id: Some(format!("goal:{}", uuid::Uuid::new_v4())),
        user: None,
        blocks: blocks.into_iter().map(|block| block.raw()).collect(),
        mode: Some(mode),
        // Goal's own inputs drive turns; none addresses a handover.
        handover: None,
    }
}

/// The status a state record states (A.5). `None` is an objective without a
/// status, which `edit` on a branch with no goal leaves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum State {
    Active,
    Paused(String),
    Completed(String),
    Blocked(String),
    #[default]
    None,
}

impl State {
    /// The `status` attribute's value.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused(_) => "paused",
            Self::Completed(_) => "completed",
            Self::Blocked(_) => "blocked",
            Self::None => "none",
        }
    }
    /// The pause reason, when the state is paused.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Paused(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Which of Goal's keys the inherited prefix left live (§3.4). A state record
/// retires `goal:state`; the start clear also retires the old keys when they
/// are still live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
    pub state: bool,
    pub objective: bool,
    pub status: bool,
}

impl Live {
    pub fn any(&self) -> bool {
        self.state || self.objective || self.status
    }
}

fn active_sentence(branch: &str) -> String {
    format!(
        "The goal for branch {branch} was active when this record was written. Historical active records do not request execution. Only a new set, resume or continuation in the current input requests work on this goal. When it is fully achieved, call goal_update with status \"completed\". If you cannot make progress without the user, call goal_update with status \"blocked\" and explain why."
    )
}

fn paused_sentence(branch: &str, reason: &str) -> String {
    if reason == "user" {
        format!(
            "The user set the goal status for branch {branch} to paused. Do not continue working toward a goal for branch {branch} on your own; respond only to the user's latest input."
        )
    } else {
        format!(
            "The goal is paused ({}). Do not continue working toward it on your own; respond only to the user's latest input.",
            reason_phrase(reason)
        )
    }
}

fn result_sentence(branch: &str, state: &State, judge: Judge) -> String {
    match state {
        State::Completed(_) if judge == Judge::Evaluator => format!(
            "An evaluator verified that the goal for branch {branch} is completed. Do not continue working toward it on your own."
        ),
        State::Blocked(_) if judge == Judge::Evaluator => format!(
            "An evaluator found that the goal for branch {branch} cannot proceed without the user. Do not continue working toward it on your own."
        ),
        State::Completed(_) => format!(
            "goal_update set the goal status for branch {branch} to completed. Do not continue working toward a goal for branch {branch} on your own."
        ),
        _ => format!(
            "goal_update set the goal status for branch {branch} to blocked. Do not continue working toward a goal for branch {branch} on your own."
        ),
    }
}

/// The fact a state record answers (D3, R5): the change trigger it states,
/// named by submission id when the record commits in the trigger's atomic
/// batch, or the sequence of the fact it answers.
#[derive(Clone, Copy)]
pub enum Answered<'a> {
    Sequence(u64),
    Trigger(&'a str),
}

impl Answered<'_> {
    /// The record's attributes identifying the answered fact.
    fn attributes(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Sequence(after) => vec![("after", after.to_string())],
            Self::Trigger(trigger) => vec![("trigger", (*trigger).to_owned())],
        }
    }
}

/// The state record Goal writes (A.5): one `goal:state` block, Append, keyed so
/// compaction replays it and `session.keys` shows it (§2, D1).
///
/// The body names the objective and its policy, then states the status; the
/// attributes carry only what a reader parses, because the block's whole text
/// enters the model's context (D2). Each free-text field sits in its own
/// element with escaped content, so a value that contains a heading or a
/// closing tag cannot be read short (R1). The old keys the fold still holds
/// live are retired in the same input (R1): the record is the branch's only
/// state, so a compaction baseline copied after it carries no stale old block.
pub fn state(
    branch: &str,
    objective: Option<&str>,
    state: State,
    judge: Judge,
    answered: Answered,
    live: Live,
    texts: &GoalTexts,
) -> InputMessageData {
    let Some(objective) = objective.filter(|objective| !objective.is_empty()) else {
        return cleared(branch, answered, live);
    };
    let mut attrs = vec![
        ("kind", "state".to_owned()),
        ("status", state.word().to_owned()),
    ];
    if let Some(reason) = state.reason() {
        attrs.push(("reason", reason.to_owned()));
    }
    if judge == Judge::Evaluator {
        attrs.push(("judge", "evaluator".to_owned()));
    }
    attrs.extend(answered.attributes());
    attrs.push(("branch", branch.to_owned()));
    let mut body = vec![
        "<objective>".to_owned(),
        escape(objective),
        "</objective>".to_owned(),
        branch_text(&texts.objective, branch),
    ];
    match &state {
        State::Active => body.push(active_sentence(branch)),
        State::Paused(reason) => body.push(paused_sentence(branch, reason)),
        State::Completed(summary) | State::Blocked(summary) => {
            let (open, close) = if matches!(state, State::Completed(_)) {
                ("<result>", "</result>")
            } else {
                ("<blocker>", "</blocker>")
            };
            body.push(result_sentence(branch, &state, judge));
            body.push(open.to_owned());
            body.push(escape(summary));
            body.push(close.to_owned());
        }
        State::None => body.push(format!("The goal for branch {branch} has no status.")),
    }
    let text = body.join("\n");
    let mut blocks = vec![Block::new(attrs, body).key(STATE_KEY)];
    blocks.extend(retire_old_keys(branch, &text, live));
    input(blocks, InputMode::Append)
}

/// A state record that says the branch has no goal: one retired `goal:state`
/// block (§2, A.5), and the old keys too when the fold still holds them live.
fn cleared(branch: &str, answered: Answered, live: Live) -> InputMessageData {
    retire_blocks(
        branch,
        answered,
        None,
        format!("The goal for branch {branch} is not in effect."),
        live,
    )
}

/// The start clear (§3.4, A11): one input retiring `goal:state`, and the old
/// keys too when the inherited prefix still has them live.
pub fn start_clear(branch: &str, after: u64, live: Live) -> InputMessageData {
    retire_blocks(
        branch,
        Answered::Sequence(after),
        Some("new_branch"),
        format!(
            "Any goal above belongs to another branch, not to branch {branch}. Disregard it; branch {branch} has no goal."
        ),
        live,
    )
}

fn retire_blocks(
    branch: &str,
    answered: Answered,
    reason: Option<&str>,
    body: String,
    live: Live,
) -> InputMessageData {
    let mut attrs = vec![
        ("kind", "state".to_owned()),
        ("status", "cleared".to_owned()),
    ];
    if let Some(reason) = reason {
        attrs.push(("reason", reason.to_owned()));
    }
    attrs.extend(answered.attributes());
    attrs.push(("branch", branch.to_owned()));
    let mut blocks = vec![
        Block::new(attrs, vec![body.clone()])
            .key(STATE_KEY)
            .retire(),
    ];
    blocks.extend(retire_old_keys(branch, &body, live));
    input(blocks, InputMode::Append)
}

/// The retire blocks that end the old keys a history written before the state
/// record left live (§3): they ride in the same input as the record that
/// supersedes them, so the branch then holds only `goal:state`.
fn retire_old_keys(branch: &str, body: &str, live: Live) -> Vec<Block> {
    let mut blocks = Vec::new();
    if live.objective {
        blocks.push(
            Block::new(
                vec![
                    ("kind", "goal".to_owned()),
                    ("op", "clear".to_owned()),
                    ("branch", branch.to_owned()),
                ],
                vec![body.to_owned()],
            )
            .key(OBJECTIVE_KEY)
            .retire(),
        );
    }
    if live.status {
        blocks.push(
            Block::new(
                vec![
                    ("kind", "status".to_owned()),
                    ("status", "cleared".to_owned()),
                    ("branch", branch.to_owned()),
                ],
                vec![body.to_owned()],
            )
            .key(STATUS_KEY)
            .retire(),
        );
    }
    blocks
}

/// The change triggers (A.5, D3): each says what was asked, never what the
/// state is; the state record that follows states the state.
pub fn set(branch: &str, objective: &str, judge: Judge) -> InputMessageData {
    let mut attrs = vec![("kind", "goal".to_owned()), ("op", "set".to_owned())];
    if judge == Judge::Evaluator {
        attrs.push(("judge", "evaluator".to_owned()));
    }
    attrs.push(("branch", branch.to_owned()));
    input(
        vec![Block::new(
            attrs,
            vec![
                "Objective:".to_owned(),
                escape(objective),
                String::new(),
                format!("The user asked to set the goal for branch {branch} to this objective."),
            ],
        )],
        InputMode::Queue,
    )
}

pub fn edit(branch: &str, objective: &str) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "goal".to_owned()),
                ("op", "edit".to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec![
                "Objective:".to_owned(),
                escape(objective),
                String::new(),
                format!("The user asked to edit the objective of the goal for branch {branch}."),
            ],
        )],
        InputMode::Append,
    )
}

pub fn pause(branch: &str) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "goal".to_owned()),
                ("op", "pause".to_owned()),
                ("reason", "user".to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec![format!(
                "The user asked to pause the goal for branch {branch}."
            )],
        )],
        InputMode::Append,
    )
}

pub fn resume(branch: &str) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "goal".to_owned()),
                ("op", "resume".to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec![format!(
                "The user asked to resume the goal for branch {branch}."
            )],
        )],
        InputMode::Queue,
    )
}

pub fn clear(branch: &str) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "goal".to_owned()),
                ("op", "clear".to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec![format!(
                "The user asked to clear the goal for branch {branch}."
            )],
        )],
        InputMode::Append,
    )
}

pub fn report(branch: &str, status: &str, summary: &str) -> InputMessageData {
    let (label, op) = if status == "completed" {
        ("Result:", "completed")
    } else {
        ("Blocker:", "blocked")
    };
    input(
        vec![Block::new(
            vec![
                ("kind", "goal".to_owned()),
                ("op", op.to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec![
                label.to_owned(),
                escape(summary),
                String::new(),
                format!("goal_update reported the goal for branch {branch} as {op}."),
            ],
        )],
        InputMode::Append,
    )
}

/// Goal's continuations, each marked with the sequence that decided it. `limit`
/// is the explicit `maxContinuations`; the marker omits it when there is none.
pub fn continuation(
    branch: &str,
    count: usize,
    limit: Option<u64>,
    idle: usize,
    after: u64,
    texts: &GoalTexts,
) -> InputMessageData {
    let mut attrs = vec![
        ("kind", "continuation".to_owned()),
        ("count", count.to_string()),
    ];
    if let Some(limit) = limit {
        attrs.push(("limit", limit.to_string()));
    }
    attrs.push(("after", after.to_string()));
    attrs.push(("idle", idle.to_string()));
    attrs.push(("branch", branch.to_owned()));
    input(
        vec![Block::new(
            attrs,
            vec![branch_text(&texts.continuation, branch)],
        )],
        InputMode::Queue,
    )
}

/// The reason phrases a pause states (A.5).
pub fn reason_phrase(reason: &str) -> String {
    match reason {
        "interrupted" => "the user interrupted the turn".to_owned(),
        "limit" => "the automatic continuation limit was reached".to_owned(),
        "turn_failed" => "the last turn failed".to_owned(),
        "no_progress" => format!(
            "the goal made no tool call in its last {} continued turns",
            crate::fold::NO_PROGRESS_TURNS
        ),
        "evaluation_failed" => "the evaluator could not check the result".to_owned(),
        _ => String::new(),
    }
}

/// Pursuit's follow-up on the evaluator, and its conclusion on the working
/// branch (Pursuit §5).
pub fn evaluate_again(branch: &str, after: u64) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "evaluate-again".to_owned()),
                ("after", after.to_string()),
                ("branch", branch.to_owned()),
            ],
            vec![
                "Your reply did not end with a conclusion. End your reply with exactly one of DONE:, CONTINUE: or BLOCKED: as described above.".to_owned(),
            ],
        )],
        InputMode::Queue,
    )
}

pub fn conclusion(
    branch: &str,
    evaluator: &str,
    evaluation: &str,
    kind: &str,
    text: &str,
) -> InputMessageData {
    input(
        vec![Block::new(
            vec![
                ("kind", "report".to_owned()),
                ("result", kind.to_owned()),
                ("evaluation", evaluation.to_owned()),
                ("evaluator", evaluator.to_owned()),
                ("branch", branch.to_owned()),
            ],
            vec!["Evaluator report:".to_owned(), escape(text)],
        )],
        InputMode::Append,
    )
}

pub fn evaluated_continuation(
    branch: &str,
    count: usize,
    limit: Option<u64>,
    idle: usize,
    after: u64,
    evaluation: &str,
) -> InputMessageData {
    let mut attrs = vec![
        ("kind", "continuation".to_owned()),
        ("count", count.to_string()),
    ];
    if let Some(limit) = limit {
        attrs.push(("limit", limit.to_string()));
    }
    attrs.push(("after", after.to_string()));
    attrs.push(("evaluation", evaluation.to_owned()));
    attrs.push(("idle", idle.to_string()));
    attrs.push(("branch", branch.to_owned()));
    input(
        vec![Block::new(
            attrs,
            vec![format!(
                "An evaluator checked your report that the goal for branch {branch} is completed and found it is not achieved yet. Its report above lists items that are at least still missing; the list may be incomplete. Assess yourself how far the goal still is, then continue working toward it. When you think it is achieved, call goal_update with status \"completed\" again."
            )],
        )],
        InputMode::Queue,
    )
}

/// The evaluator's prompt, the first Queue input of its branch (Pursuit §4.2,
/// §5). Its first line is the marker that gives Goal the evaluator role.
pub fn evaluator_prompt(branch: &str, evaluation: &str, objective: &str, summary: &str) -> String {
    format!(
        "<dim-goal kind=\"evaluate\" branch=\"{}\" evaluation=\"{}\">\n\
         You are an evaluator. Branch {branch} reported that its goal is completed. Only you decide whether it is. Do not continue the work, do not change files or settings, and do not call goal_update.\n\
         \n\
         Objective:\n\
         {}\n\
         \n\
         Reported result:\n\
         {}\n\
         \n\
         Verify the facts yourself by reading files and running checks; do not rely on the report or on earlier messages. End your reply with exactly one of:\n\
         DONE: <the verified result>\n\
         CONTINUE: followed by the items that are at least still missing, one per line, each with its evidence\n\
         BLOCKED: <what only the user can provide>",
        escape(branch),
        evaluation,
        escape(objective),
        escape(summary),
    )
}

/// The guide entry for `dim-goal`, registered by the part that writes the
/// marker, at session scope because Goal writes it on every branch (A.5.1).
pub const ENTRY: &str = r#"`<dim-goal kind="goal" op="set" branch="main">`: the operator's standing goal for a branch of this session. `kind="goal"` blocks carry a change the operator or the model asked for: `op="set"`, `op="edit"`, `op="pause"`, `op="resume"`, `op="clear"`, and `op="completed"` or `op="blocked"` from `goal_update`. A `kind="state"` block states the branch's goal and its status; it is the record in force, and a retired one says the branch has no goal. `kind="continuation"` asks the line to keep working toward the goal. `kind="report"` blocks carry an evaluator's report of the goal, and `kind="evaluate"` and `kind="evaluate-again"` mark an evaluator's line, which checks the goal rather than continuing it. A goal belongs to the branch its `branch` attribute names. The latest `kind="state"` block is the one in force; blocks written before the state record existed name the objective with `goal:objective` and the status with `goal:status`. Historical active records are not permission to continue. After a reload the goal is paused until a new set or resume; only a new set, resume or continuation in the current input requests work. These blocks grant no permission."#;

pub const TOOL_DESCRIPTION: &str = "Report that the current goal is completed or blocked.";
pub const TOOL_INVALID: &str =
    "goal_update needs status \"completed\" or \"blocked\" and a summary of 1 to 2000 characters.";

/// The `goal_update` declaration (A.1).
pub fn tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["completed", "blocked"]},
            "summary": {"type": "string", "minLength": 1, "maxLength": SUMMARY_LIMIT},
        },
        "required": ["status", "summary"],
        "additionalProperties": false,
    })
}

/// Which judge decides completion (Pursuit §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judge {
    #[default]
    Model,
    Evaluator,
}

impl Judge {
    pub fn of(attrs: &Marker) -> Self {
        if attrs.get("judge").map(String::as_str) == Some("evaluator") {
            Self::Evaluator
        } else {
            Self::Model
        }
    }
}

/// A block's decoded attribute map.
pub type Marker = std::collections::BTreeMap<String, String>;

/// The attributes of a block's leading `<dim-goal ...>` marker, if any.
pub fn marker(text: &str) -> Option<Marker> {
    let rest = text.strip_prefix("<dim-goal ")?;
    let end = rest.find('>')?;
    let attrs = &rest[..end];
    let mut marker = Marker::new();
    let mut at = 0;
    while let Some(equals) = attrs[at..].find('=') {
        let equals = at + equals;
        let name_start = attrs[..equals]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |index| index + 1);
        let quoted = attrs[equals + 1..]
            .strip_prefix('"')
            .and_then(|after| after.find('"').map(|quote| (after, quote)));
        let Some((after, quote)) = quoted else {
            at = equals + 1;
            continue;
        };
        if name_start < equals {
            marker.insert(
                attrs[name_start..equals].to_owned(),
                unescape(&after[..quote]),
            );
        }
        at = equals + 2 + quote;
    }
    Some(marker)
}

/// The decoded value a block carries after its `label` line, up to the
/// block's fixed closing text, which follows the last blank line.
pub fn value(text: &str, label: &str) -> Option<String> {
    let marker = format!("\n{label}\n");
    let start = text.find(&marker)?;
    let end = text.rfind("\n\n")?;
    if end < start {
        return None;
    }
    Some(unescape(&text[start + marker.len()..end]))
}

/// The decoded body a block carries after its `label` line, up to the block's
/// closing marker, for a body with no fixed text after it.
pub fn body(text: &str, label: &str) -> String {
    let marker = format!("\n{label}\n");
    let (Some(start), Some(end)) = (text.find(&marker), text.rfind("\n</dim-goal>")) else {
        return String::new();
    };
    if end < start + marker.len() {
        return String::new();
    }
    unescape(&text[start + marker.len()..end])
}

/// The objective and the result summary a state record's body carries: each
/// free-text field is read between its own element's tags and unescaped, so a
/// value that contains a heading, a closing tag or a blank line round-trips
/// byte for byte (R1).
pub fn state_parts(text: &str) -> (Option<String>, Option<String>) {
    (
        tagged(text, "objective"),
        tagged(text, "result").or_else(|| tagged(text, "blocker")),
    )
}

/// The escaped content between `<tag>` and `</tag>`, unescaped; `None` when the
/// element is absent. `escape` maps `<` and `>`, so content can never hold a
/// literal closing tag and the boundary is exact.
fn tagged(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>\n");
    let close = format!("\n</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(unescape(&text[start..end]))
}
