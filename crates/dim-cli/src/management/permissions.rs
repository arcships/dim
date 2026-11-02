//! The Command rules area: the user's command rules through the same
//! operations the line prompt uses (`permission.rules`, `permission.set_rules`).
//!
//! A rule has no id of its own, so its id and label are its value,
//! `DECISION MATCH COMMAND`. A remove therefore acts on exactly the rule the
//! user saw; a change to some other rule is not a conflict, and only a rule
//! that vanished or a write the owner raced is (§ The command rules area).

use dim_agent_types::{CommandMatch, CommandRule, CommandRuleDecision};

use super::Groups;
use crate::client::Client;
use crate::model::session::{ActionKind, ManagementAction, ManagementEntry, ManagementView};
use crate::permission::{self, Added, Removed, Rules};
use crate::ui::i18n::{t, tf};

/// The rule a panel entry, a remove and the CLI's argument name:
/// `DECISION MATCH COMMAND`.
fn rule_id(rule: &CommandRule) -> String {
    format!(
        "{} {} {}",
        permission::decision_name(rule.decision),
        permission::match_name(rule.matching),
        rule.command
    )
}

/// The rule `id` names, checked as the line prompt's add checks one.
fn parse_rule(id: &str) -> Result<CommandRule, Failure> {
    let words: Vec<&str> = id.split_whitespace().collect();
    let Some((decision, matching, command)) = permission::parse_rule_words(&words) else {
        return Err(named_rule());
    };
    permission::rule(&command, matching, decision)
        .map_err(|error| Failure::Refused(error.to_string()))
}

fn named_rule() -> Failure {
    Failure::Refused(
        t("A command rule is DECISION MATCH COMMAND, such as deny prefix git push.").to_owned(),
    )
}

/// What a Command rules action came to.
pub enum Answer {
    /// The owner's list after the action, with the note an add or a remove
    /// leaves.
    Rules { rules: Rules, note: Option<String> },
}

/// What a request the adapter cannot do came to.
pub enum Failure {
    /// The remove could not act on the list as it is now, and nothing was
    /// removed. `rules` is the list read when the change was found.
    Conflict { message: String, rules: Rules },
    /// Nothing was written, and this is why.
    Refused(String),
}

/// Performs `action` on the user's command rules.
pub async fn apply(client: &mut Client, action: &ManagementAction) -> Result<Answer, Failure> {
    match action {
        ManagementAction::List | ManagementAction::Reload => read(client).await,
        ManagementAction::Add { document } => add(client, document).await,
        ManagementAction::Remove { id } => remove(client, id).await,
        ManagementAction::Toggle { .. }
        | ManagementAction::Save { .. }
        | ManagementAction::Run { .. }
        | ManagementAction::Inspect { .. } => Err(Failure::Refused(
            "A command rule has no such action.".to_owned(),
        )),
    }
}

async fn read(client: &mut Client) -> Result<Answer, Failure> {
    let rules = permission::rules(client).await.map_err(refused)?;
    Ok(Answer::Rules { rules, note: None })
}

/// Appends one rule. An equal rule writes nothing and says so; a revision
/// conflict re-reads and retries inside [`permission::add_rule`], because an
/// append overwrites nothing.
async fn add(client: &mut Client, document: &serde_json::Value) -> Result<Answer, Failure> {
    let rule: CommandRule = serde_json::from_value(document.clone())
        .map_err(|error| Failure::Refused(error.to_string()))?;
    let rule = permission::rule(&rule.command, rule.matching, rule.decision)
        .map_err(|error| Failure::Refused(error.to_string()))?;
    let described = permission::describe(&rule);
    match permission::add_rule(client, rule).await.map_err(refused)? {
        Added::Added(rules) => Ok(Answer::Rules {
            rules,
            note: Some(tf("Rule added: {rule}.", &[("rule", &described)])),
        }),
        // Nothing was written; the list the note comes with is the owner's
        // list now, so it is read once more.
        Added::Present => {
            let rules = permission::rules(client).await.map_err(refused)?;
            Ok(Answer::Rules {
                rules,
                note: Some(tf("Rule already present: {rule}.", &[("rule", &described)])),
            })
        }
    }
}

/// Removes the rule `id` names. The adapter reads the list first, finds the
/// rule by value and writes at that read's revision: a rule that is gone, or
/// a write another client raced, writes nothing and reports the conflict
/// with the list just read.
async fn remove(client: &mut Client, id: &str) -> Result<Answer, Failure> {
    let wanted = parse_rule(id)?;
    let seen = permission::rules(client).await.map_err(refused)?;
    let Some(index) = seen
        .command_rules
        .iter()
        .position(|rule| permission::same_rule(rule, &wanted))
    else {
        return Err(Failure::Conflict {
            message: t("That command rule is no longer listed; nothing was removed.").to_owned(),
            rules: seen,
        });
    };
    match permission::remove_rule(client, &seen, index)
        .await
        .map_err(refused)?
    {
        Removed::Removed { rule, rules } => Ok(Answer::Rules {
            rules,
            note: Some(tf(
                "Rule removed: {rule}.",
                &[("rule", &permission::describe(&rule))],
            )),
        }),
        Removed::Conflict(rules) => Err(Failure::Conflict {
            message: t("Command rules changed since they were read; nothing was removed.")
                .to_owned(),
            rules,
        }),
    }
}

fn refused(error: crate::Error) -> Failure {
    Failure::Refused(error.to_string())
}

/// The area as its owner reports it: one entry per rule under the one scope
/// it has, with the kinds the daemon advertises.
pub(super) fn view(rules: &Rules, actions: &[ActionKind]) -> ManagementView {
    let mut groups = Groups::default();
    for rule in &rules.command_rules {
        let id = rule_id(rule);
        groups.push(
            t("user"),
            ManagementEntry {
                id: id.clone(),
                label: id,
                detail: detail(rule),
                // A rule has a decision, not an on/off state.
                enabled: None,
                states: Vec::new(),
                actions: super::entry_kinds(actions),
                document: None,
            },
        );
    }
    ManagementView {
        // Nothing in this area writes at the view's revision: the one write
        // replaces the whole list, and a remove re-reads before it.
        revision: None,
        // The owner's one write replaces the whole list; an area-level edit
        // would be a whole-list editor, which only the Web page is.
        document: None,
        create: Some(create()),
        actions: super::area_kinds(actions),
        groups: groups.into_vec(),
        issues: Vec::new(),
    }
}

/// What a new rule starts from: the owner's write unit with a command no one
/// would keep, so a document left as it is reads as the example it is rather
/// than as a rule the user meant. The other two members are valid values, so
/// the document decodes and a user who types only the command still denies.
fn create() -> serde_json::Value {
    serde_json::json!({
        "command": "replace-with-a-command",
        "match": "prefix",
        "decision": "deny"
    })
}

/// What the selected rule says about itself: the command in full, how it
/// matches, what it decides, and where it applies.
fn detail(rule: &CommandRule) -> Vec<String> {
    let matching = match rule.matching {
        CommandMatch::Exact => t("Matches only this command."),
        CommandMatch::Prefix => t("Matches commands whose first words are these."),
    };
    let decision = match rule.decision {
        CommandRuleDecision::Allow => t("Allows a simple command that would otherwise ask."),
        CommandRuleDecision::Deny => t("Denies the command under every preset."),
    };
    vec![
        // The row is one line and the confirmation repeats it, so the
        // command in full is here, where the footer wraps it.
        tf("Command: {command}", &[("command", &rule.command)]),
        matching.to_owned(),
        decision.to_owned(),
        t("Applies in every Session, in every workspace.").to_owned(),
    ]
}
