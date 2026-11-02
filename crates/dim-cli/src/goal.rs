//! A branch's goal, folded from its events, and the `/goal` command's forms
//! (Goal design §A.3 and `docs/design/cli-commands.md`).

use dim_agent_types::InputBlock;

use crate::client::Client;
use crate::wire::{Event, EventContent};

/// The key of the objective block and of the status block (Goal §A.2).
const OBJECTIVE_KEY: &str = "goal:objective";
const STATUS_KEY: &str = "goal:status";

/// The goal in force on one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub objective: String,
    pub status: Status,
    /// Pursuit's evaluator judge: another line decides when the goal is done.
    pub evaluator: bool,
}

/// The status in force, as the latest live `goal:status` block gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Active,
    Paused(String),
    Completed,
    Blocked,
    /// An objective is live and no status block is: `edit` without a goal,
    /// or a status block older than the latest `clear`.
    None,
}

impl Status {
    /// `active`, `paused (limit)`, `completed`, `blocked` or `no status`,
    /// with the pause reason kept when Goal paused the goal itself.
    pub fn words(&self) -> String {
        match self {
            Self::Active => "active".into(),
            Self::Paused(reason) => format!("paused ({})", reason.replace('_', " ")),
            Self::Completed => "completed".into(),
            Self::Blocked => "blocked".into(),
            Self::None => "no status".into(),
        }
    }
}

/// One goal block the line's own events carry, kept so a deletion can be
/// folded out again (Goal §2).
enum Block {
    /// `goal:objective`, written by `set` (`set`) or by `edit`; `text` is
    /// none when the block carries no `Objective:` line.
    Objective {
        input: String,
        text: Option<String>,
        set: bool,
        judge: Option<String>,
    },
    /// `goal:objective`, retired by `clear`.
    ObjectiveRetire { input: String },
    /// `goal:status` in force.
    Status {
        input: String,
        status: Status,
        by_evaluator: bool,
    },
    /// `goal:status`, retired.
    StatusRetire { input: String },
}

impl Block {
    fn input(&self) -> &str {
        match self {
            Self::Objective { input, .. }
            | Self::ObjectiveRetire { input }
            | Self::Status { input, .. }
            | Self::StatusRetire { input } => input,
        }
    }
}

/// The goal of one branch, folded from its own events (Goal §2, A.3): the
/// fold starts at the line's opening, the latest live block of each key
/// decides, `clear` retires both keys, `edit` keeps the judge, and a deleted
/// input's blocks leave the fold. Clients read no other state and ask Goal
/// for nothing.
#[derive(Default)]
pub struct Fold {
    /// The goal blocks of the line's own events, in event order.
    blocks: Vec<Block>,
    /// Inputs an `input/deleted` removed; their blocks are folded out.
    deleted: std::collections::HashSet<String>,
}

impl Fold {
    /// Folds one event of the branch this fold belongs to.
    pub fn apply(&mut self, event: &Event) {
        match &event.content {
            // The line's own opening: what came before it is inherited or
            // copied and never contributes a goal (Goal §2, A11).
            EventContent::BranchStart(_) | EventContent::SessionForked(_) => self.blocks.clear(),
            EventContent::Input(data) => {
                for block in &data.blocks {
                    let InputBlock::Raw { text, key, retire } = block;
                    let Some(key) = key
                        .as_deref()
                        .filter(|key| [OBJECTIVE_KEY, STATUS_KEY].contains(key))
                    else {
                        continue;
                    };
                    // Goal reads a block only when it opens with a marker, the
                    // retired ones included, whatever its `kind` says.
                    if marker(text).is_none() {
                        continue;
                    }
                    if *retire {
                        self.blocks.push(if key == OBJECTIVE_KEY {
                            Block::ObjectiveRetire {
                                input: event.id.clone(),
                            }
                        } else {
                            Block::StatusRetire {
                                input: event.id.clone(),
                            }
                        });
                        continue;
                    }
                    if key == OBJECTIVE_KEY {
                        self.blocks.push(Block::Objective {
                            input: event.id.clone(),
                            text: value(text, "Objective:"),
                            set: attribute(text, "op").as_deref() == Some("set"),
                            judge: attribute(text, "judge"),
                        });
                        continue;
                    }
                    let Some(status) = status_of(text) else {
                        continue;
                    };
                    self.blocks.push(Block::Status {
                        input: event.id.clone(),
                        status,
                        by_evaluator: attribute(text, "by").as_deref() == Some("evaluator"),
                    });
                }
            }
            // Only an input this fold keeps a block for can change the goal;
            // a deletion always follows its input, so the block is already
            // here. Holding every id the line deletes would grow with the
            // line, not with its goal edits.
            EventContent::InputDeleted(data)
                if self
                    .blocks
                    .iter()
                    .any(|block| block.input() == data.input_id) =>
            {
                self.deleted.insert(data.input_id.clone());
            }
            _ => {}
        }
    }

    pub fn goal(&self) -> Option<Goal> {
        let mut objective = None;
        let mut judge = false;
        let mut status = None;
        for block in &self.blocks {
            if self.deleted.contains(block.input()) {
                continue;
            }
            match block {
                Block::Objective {
                    text,
                    set,
                    judge: made,
                    ..
                } => {
                    objective = text.clone();
                    if *set {
                        judge = made.as_deref() == Some("evaluator");
                    }
                }
                Block::ObjectiveRetire { .. } => {
                    objective = None;
                    judge = false;
                }
                Block::Status {
                    status: value,
                    by_evaluator,
                    ..
                } => {
                    // A16: under the evaluator judge a worker's `completed`
                    // is a request; only the evaluator completes the goal.
                    if matches!(value, Status::Completed) && !by_evaluator && judge {
                        continue;
                    }
                    status = Some(value.clone());
                }
                Block::StatusRetire { .. } => status = None,
            }
        }
        objective.map(|objective| Goal {
            objective,
            status: status.unwrap_or(Status::None),
            evaluator: judge,
        })
    }
}

/// The status a live `goal:status` block sets (Goal A.3): a `kind="status"`
/// block says active, a `kind="goal"` block says its `op`.
fn status_of(text: &str) -> Option<Status> {
    if attribute(text, "kind").as_deref() == Some("status") {
        return Some(Status::Active);
    }
    Some(match attribute(text, "op").as_deref()? {
        "pause" => Status::Paused(attribute(text, "reason").unwrap_or_else(|| "user".into())),
        "resume" => Status::Active,
        "completed" => Status::Completed,
        "blocked" => Status::Blocked,
        _ => return None,
    })
}

/// The value after a block's `label` line, up to its last blank line, with
/// the marker's escaping reversed; none when the block has no such line
/// (Goal's `value()`).
fn value(text: &str, label: &str) -> Option<String> {
    let start = text.find(&format!("\n{label}\n"))?;
    let end = text.rfind("\n\n")?;
    if end < start {
        return None;
    }
    Some(unescape(
        text.get(start + label.len() + 2..end).unwrap_or(""),
    ))
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// The attributes of a block's leading `<dim-goal ...>` marker, if the block
/// opens with one (Goal's `marker()`).
fn marker(text: &str) -> Option<&str> {
    text.strip_prefix("<dim-goal ")?
        .split_once('>')
        .map(|(attrs, _)| attrs)
}

/// The value of `name="value"` in the block's marker, read as Goal's
/// `(\w+)="([^"]*)"` reads it.
fn attribute(text: &str, name: &str) -> Option<String> {
    let attrs = marker(text)?;
    let mut offset = 0;
    while let Some(found) = attrs[offset..].find("=\"") {
        let eq = offset + found;
        let start = attrs[..eq]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |at| at + 1);
        let quote = eq + 2;
        let end = attrs[quote..].find('"')? + quote;
        if &attrs[start..eq] == name {
            return Some(unescape(&attrs[quote..end]));
        }
        offset = end + 1;
    }
    None
}

/// The words after `/goal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The goal in force, or that the branch has none.
    Show,
    /// An objective typed without a method: `edit` while the goal in force
    /// is active or paused, so only the objective changes, and `set`
    /// otherwise, which starts a new goal.
    Objective(String),
    Set(String),
    Edit(String),
    Pause,
    Resume,
    Clear,
}

/// The method a request writes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Set,
    Edit,
    Pause,
    Resume,
    Clear,
}

impl Call {
    pub fn method(self) -> &'static str {
        match self {
            Self::Set => "session.goal.set",
            Self::Edit => "session.goal.edit",
            Self::Pause => "session.goal.pause",
            Self::Resume => "session.goal.resume",
            Self::Clear => "session.goal.clear",
        }
    }

    /// The word the command's output names the call with.
    pub fn words(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Edit => "edited",
            Self::Pause => "paused",
            Self::Resume => "resumed",
            Self::Clear => "cleared",
        }
    }
}

/// What `/goal` does with the words after it; `None` is a usage error.
///
/// `pause`, `resume` and `clear` name their method; `set` and `edit` name it
/// and take the objective after it, so an objective that is one of those
/// words is written as `/goal set pause`.
pub fn request(words: &str) -> Option<Request> {
    let words = words.trim();
    if words.is_empty() {
        return Some(Request::Show);
    }
    let (word, rest) = match words.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (words, ""),
    };
    Some(match word {
        "pause" if rest.is_empty() => Request::Pause,
        "resume" if rest.is_empty() => Request::Resume,
        "clear" if rest.is_empty() => Request::Clear,
        "set" => return (!rest.is_empty()).then(|| Request::Set(rest.to_owned())),
        "edit" => return (!rest.is_empty()).then(|| Request::Edit(rest.to_owned())),
        _ => Request::Objective(words.to_owned()),
    })
}

/// The objective a writing request carries.
pub fn objective(request: &Request) -> Option<&str> {
    match request {
        Request::Objective(text) | Request::Set(text) | Request::Edit(text) => Some(text),
        _ => None,
    }
}

/// The call a request makes, given the goal in force on the branch.
///
/// A typed objective edits only a goal that is still being worked toward
/// (active or paused); with no goal, no status, or a finished one, it sets a
/// new goal, which is what starts the branch working again (Goal §2).
pub fn call(request: &Request, goal: Option<&Goal>) -> Option<Call> {
    Some(match request {
        Request::Show => return None,
        Request::Objective(_)
            if goal
                .is_some_and(|goal| matches!(goal.status, Status::Active | Status::Paused(_))) =>
        {
            Call::Edit
        }
        Request::Objective(_) | Request::Set(_) => Call::Set,
        Request::Edit(_) => Call::Edit,
        Request::Pause => Call::Pause,
        Request::Resume => Call::Resume,
        Request::Clear => Call::Clear,
    })
}

/// Whether this daemon installs Goal: the Session's method table
/// (`session.methods`) lists `session.goal.set`, which Goal registers when
/// its Session part is loaded. A daemon that does not advertise the table is
/// trusted until the call itself answers `unknown_method`.
pub async fn available(client: &mut Client, session_id: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Decl {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Table {
        methods: Vec<Decl>,
    }
    if !client.offers("session.methods") {
        return true;
    }
    match client
        .typed::<Table>(
            "session.methods",
            serde_json::json!({"sessionId": session_id}),
        )
        .await
    {
        Ok(table) => table
            .methods
            .iter()
            .any(|decl| decl.name == Call::Set.method()),
        Err(_) => true,
    }
}

/// What `/goal` says when this daemon installs no Goal.
pub const NOT_AVAILABLE: &str = "Goal is not available on this daemon.";

/// What `/goal` prints when its words name no form.
pub const USAGE: &str =
    "Usage: /goal [OBJECTIVE | set OBJECTIVE | edit OBJECTIVE | pause | resume | clear]";

/// What `/goal` prints for `branch`: the call it made, none for the read,
/// and the state the branch's events give through the call.
pub fn report(branch: &str, call: Option<Call>, goal: Option<&Goal>) -> Vec<String> {
    let Some(goal) = goal else {
        return vec![match call {
            Some(Call::Clear) => format!("Goal cleared on {branch}."),
            _ => format!("No goal on {branch}."),
        }];
    };
    let mut first = match call {
        Some(call) => format!("Goal {} on {branch}", call.words()),
        None => format!("Goal on {branch}"),
    };
    first.push_str(" · ");
    first.push_str(&goal.status.words());
    if goal.evaluator {
        first.push_str(" · evaluator judge");
    }
    let mut lines = vec![first];
    for line in goal.objective.lines() {
        lines.push(format!("  {line}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn input(sequence: u64, blocks: Value) -> Event {
        serde_json::from_value(json!({
            "id": format!("input-{sequence}"), "sequence": sequence, "create_at": 1,
            "type": "input", "name": "message", "data": {"blocks": blocks},
        }))
        .unwrap()
    }

    fn set(sequence: u64) -> Event {
        input(
            sequence,
            json!([
                {"type":"raw","key":OBJECTIVE_KEY,"text":
                    "<dim-goal kind=\"goal\" op=\"set\" branch=\"main\">\nObjective:\nMake the tests pass\n\nThis is the user's standing goal for branch main.\n</dim-goal>"},
                {"type":"raw","key":STATUS_KEY,"text":
                    "<dim-goal kind=\"status\" status=\"active\" branch=\"main\">\nThe goal for branch main is active.\n</dim-goal>"},
            ]),
        )
    }

    fn change(sequence: u64, op: &str, key: &str, extra: &str, retire: bool) -> Event {
        let body = match op {
            "pause" => "The goal is paused.\n</dim-goal>".to_owned(),
            "completed" | "blocked" => {
                format!("Result:\nDone {sequence}\n\nFinished.\n</dim-goal>")
            }
            "clear" => {
                "The previous goal was cleared by the user. Disregard it.\n</dim-goal>".into()
            }
            _ => format!("Objective:\nObjective {sequence}\n\nChanged.\n</dim-goal>"),
        };
        input(
            sequence,
            json!([{"type":"raw","key":key,"retire":retire,
                "text":format!("<dim-goal kind=\"goal\" op=\"{op}\"{extra} branch=\"main\">\n{body}")}]),
        )
    }

    fn deleted(sequence: u64, input: &str) -> Event {
        serde_json::from_value(json!({
            "id": format!("deleted-{sequence}"), "sequence": sequence, "create_at": 1,
            "type": "input", "name": "deleted", "data": {"input_id": input},
        }))
        .unwrap()
    }

    fn start(sequence: u64) -> Event {
        serde_json::from_value(json!({
            "id": format!("start-{sequence}"), "sequence": sequence, "create_at": 1,
            "type": "branch", "name": "start", "data": {"parent": "main"},
        }))
        .unwrap()
    }

    fn forked(sequence: u64) -> Event {
        serde_json::from_value(json!({
            "id": format!("forked-{sequence}"), "sequence": sequence, "create_at": 1,
            "type": "session", "name": "forked", "data": {},
        }))
        .unwrap()
    }

    fn evaluator_set(sequence: u64) -> Event {
        input(
            sequence,
            json!([
                {"type":"raw","key":OBJECTIVE_KEY,"text":
                    "<dim-goal kind=\"goal\" op=\"set\" judge=\"evaluator\" branch=\"main\">\nObjective:\nShip it\n\nGoal.\n</dim-goal>"},
                {"type":"raw","key":STATUS_KEY,"text":
                    "<dim-goal kind=\"status\" status=\"active\" branch=\"main\">\nThe goal for branch main is active.\n</dim-goal>"},
            ]),
        )
    }

    fn cleared(sequence: u64) -> Event {
        input(
            sequence,
            json!([
                {"type":"raw","key":OBJECTIVE_KEY,"retire":true,"text":
                    "<dim-goal kind=\"goal\" op=\"clear\" branch=\"main\">\nThe previous goal was cleared by the user. Disregard it.\n</dim-goal>"},
                {"type":"raw","key":STATUS_KEY,"retire":true,"text":
                    "<dim-goal kind=\"status\" status=\"cleared\" branch=\"main\">\nThe goal for branch main has no status.\n</dim-goal>"},
            ]),
        )
    }

    #[test]
    fn the_latest_live_block_of_each_key_is_the_goal() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        assert_eq!(
            fold.goal(),
            Some(Goal {
                objective: "Make the tests pass".into(),
                status: Status::Active,
                evaluator: false,
            })
        );
        fold.apply(&change(11, "edit", OBJECTIVE_KEY, "", false));
        assert_eq!(fold.goal().unwrap().objective, "Objective 11");
        assert_eq!(fold.goal().unwrap().status, Status::Active);
        fold.apply(&change(12, "pause", STATUS_KEY, " reason=\"limit\"", false));
        assert_eq!(fold.goal().unwrap().status, Status::Paused("limit".into()));
    }

    #[test]
    fn a_clear_retires_both_keys_and_a_deleted_change_never_counts() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        fold.apply(&change(11, "edit", OBJECTIVE_KEY, "", false));
        fold.apply(&deleted(12, "input-11"));
        assert_eq!(fold.goal().unwrap().objective, "Make the tests pass");
        fold.apply(&change(13, "clear", OBJECTIVE_KEY, "", true));
        fold.apply(&change(14, "clear", STATUS_KEY, "", true));
        assert_eq!(fold.goal(), None);
    }

    #[test]
    fn only_a_deletion_of_a_goal_block_is_kept() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        // An input with no goal block changes nothing and is not kept: the
        // set grows with the line's goal edits, not with its deletions.
        fold.apply(&deleted(11, "input-99"));
        assert!(fold.deleted.is_empty());
        assert_eq!(fold.goal().unwrap().objective, "Make the tests pass");
        // A goal block's own input still hides it when deleted.
        fold.apply(&deleted(12, "input-10"));
        assert!(fold.deleted.contains("input-10"));
        assert_eq!(fold.goal(), None);
    }

    #[test]
    fn an_objective_without_a_status_has_none() {
        let mut fold = Fold::default();
        fold.apply(&change(10, "edit", OBJECTIVE_KEY, "", false));
        assert_eq!(fold.goal().unwrap().status, Status::None);
    }

    #[test]
    fn a_set_by_an_evaluator_names_the_judge() {
        let mut fold = Fold::default();
        fold.apply(&input(
            10,
            json!([{"type":"raw","key":OBJECTIVE_KEY,"text":
                "<dim-goal kind=\"goal\" op=\"set\" judge=\"evaluator\" branch=\"main\">\nObjective:\nShip it\n\nGoal.\n</dim-goal>"}]),
        ));
        assert!(fold.goal().unwrap().evaluator);
    }

    #[test]
    fn an_escaped_objective_reads_back_verbatim() {
        let mut fold = Fold::default();
        fold.apply(&input(
            10,
            json!([{"type":"raw","key":OBJECTIVE_KEY,"text":
                "<dim-goal kind=\"goal\" op=\"set\" branch=\"main\">\nObjective:\n&lt;/dim-goal&gt; &amp; &quot;quoted&quot;\n\nGoal.\n</dim-goal>"}]),
        ));
        assert_eq!(fold.goal().unwrap().objective, "</dim-goal> & \"quoted\"");
    }

    #[test]
    fn an_objective_block_without_a_kind_folds_like_goal() {
        // Goal reads every block that opens with a marker, whatever its
        // `kind` attribute says (goal.ts `:393`).
        let mut fold = Fold::default();
        fold.apply(&input(
            10,
            json!([{"type":"raw","key":OBJECTIVE_KEY,"text":
                "<dim-goal op=\"set\" branch=\"main\">\nObjective:\nShip it\n\nGoal.\n</dim-goal>"}]),
        ));
        assert_eq!(fold.goal().unwrap().objective, "Ship it");
    }

    #[test]
    fn a_block_with_no_objective_line_yields_no_goal() {
        // Goal's `value()` returns undefined there, so the fold holds no
        // objective even though the block is live.
        let mut fold = Fold::default();
        fold.apply(&input(
            10,
            json!([{"type":"raw","key":OBJECTIVE_KEY,"text":
                "<dim-goal kind=\"goal\" op=\"set\" branch=\"main\">\nNo objective line here.\n</dim-goal>"}]),
        ));
        assert_eq!(fold.goal(), None);
    }

    #[test]
    fn a_fork_starts_the_fold_after_the_copied_goal() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        fold.apply(&forked(11));
        assert_eq!(fold.goal(), None);
    }

    #[test]
    fn a_child_line_starts_its_fold_at_its_branch_start() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        fold.apply(&start(11));
        assert_eq!(fold.goal(), None);
        fold.apply(&change(12, "set", OBJECTIVE_KEY, "", false));
        assert_eq!(fold.goal().unwrap().objective, "Objective 12");
    }

    #[test]
    fn an_evaluator_judged_goal_keeps_its_judge_through_an_edit() {
        let mut fold = Fold::default();
        fold.apply(&evaluator_set(10));
        assert!(fold.goal().unwrap().evaluator);
        fold.apply(&change(11, "edit", OBJECTIVE_KEY, "", false));
        assert_eq!(fold.goal().unwrap().objective, "Objective 11");
        assert!(fold.goal().unwrap().evaluator);
    }

    #[test]
    fn a_workers_completed_waits_for_the_evaluator() {
        let mut fold = Fold::default();
        fold.apply(&evaluator_set(10));
        fold.apply(&change(11, "completed", STATUS_KEY, "", false));
        assert_eq!(fold.goal().unwrap().status, Status::Active);
        fold.apply(&change(
            12,
            "completed",
            STATUS_KEY,
            " by=\"evaluator\"",
            false,
        ));
        assert_eq!(fold.goal().unwrap().status, Status::Completed);
    }

    #[test]
    fn deleting_the_clear_restores_the_goal_before_it() {
        let mut fold = Fold::default();
        fold.apply(&set(10));
        fold.apply(&cleared(11));
        assert_eq!(fold.goal(), None);
        fold.apply(&deleted(12, "input-11"));
        assert_eq!(
            fold.goal(),
            Some(Goal {
                objective: "Make the tests pass".into(),
                status: Status::Active,
                evaluator: false,
            })
        );
    }

    #[test]
    fn a_typed_objective_edits_only_a_live_goal() {
        let goal = |status| Goal {
            objective: "Ship it".into(),
            status,
            evaluator: false,
        };
        let request = Request::Objective("Next thing".into());
        assert_eq!(call(&request, None), Some(Call::Set));
        assert_eq!(call(&request, Some(&goal(Status::None))), Some(Call::Set));
        assert_eq!(
            call(&request, Some(&goal(Status::Active))),
            Some(Call::Edit)
        );
        assert_eq!(
            call(&request, Some(&goal(Status::Paused("limit".into())))),
            Some(Call::Edit)
        );
        assert_eq!(
            call(&request, Some(&goal(Status::Completed))),
            Some(Call::Set)
        );
        assert_eq!(
            call(&request, Some(&goal(Status::Blocked))),
            Some(Call::Set)
        );
    }

    #[test]
    fn the_words_name_the_methods_and_an_objective_spelled_like_one() {
        assert_eq!(request(""), Some(Request::Show));
        assert_eq!(request("  "), Some(Request::Show));
        assert_eq!(request("pause"), Some(Request::Pause));
        assert_eq!(request("resume"), Some(Request::Resume));
        assert_eq!(request("clear"), Some(Request::Clear));
        assert_eq!(
            request("pause now"),
            Some(Request::Objective("pause now".into()))
        );
        assert_eq!(request("edit"), None);
        assert_eq!(request("set"), None);
        assert_eq!(request("set pause"), Some(Request::Set("pause".into())));
        assert_eq!(
            request("edit Ship it"),
            Some(Request::Edit("Ship it".into()))
        );
        assert_eq!(
            request("Ship it"),
            Some(Request::Objective("Ship it".into()))
        );
        assert_eq!(
            call(&Request::Objective("Ship it".into()), None),
            Some(Call::Set)
        );
        assert_eq!(call(&Request::Set("pause".into()), None), Some(Call::Set));
        assert_eq!(call(&Request::Show, None), None);
    }
}
