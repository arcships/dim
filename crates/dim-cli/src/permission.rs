//! The permission operations of the CLI's clients, over existing App Server
//! methods: the `main` branch preset (`session.permission.get/update`), the
//! user's command rules (`permission.rules/set_rules`) and "always allow"
//! (F2 §5.4). Nothing here reads the terminal or prints: each operation
//! returns what happened and the caller shows it. The App Server decides and
//! enforces every call; a client only selects and writes.
//!
//! "Always allow" for a simple command:
//!
//! ```text
//! always_command(call)            the simple exec/execute command; None: once only
//!  └► proposed(command, typed)    the exact command, or a prefix of its words
//!      └► AddRule / add_rule      permission.rules → permission.set_rules;
//!          │                      a revision conflict reads again and retries
//!          ├ Ok(Added | Present) ► allow(..), if the approval is still open
//!          └ Err ► no answer: the approval stays open for once or deny
//! ```
use crate::{
    Result,
    client::{self, Client},
};
use dim_agent_types::{
    ApprovalCallData, COMMAND_SEPARATORS, CommandMatch, CommandRule, CommandRuleDecision,
    PermissionChangedData, PermissionPolicy, PermissionPreset,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

pub const PRESETS: [PermissionPreset; 3] = [
    PermissionPreset::ReadOnly,
    PermissionPreset::WorkspaceWrite,
    PermissionPreset::FullAccess,
];
pub const PRESET_METHODS: [&str; 2] = ["session.permission.get", "session.permission.update"];
pub const RULE_METHODS: [&str; 2] = ["permission.rules", "permission.set_rules"];
/// Attempts for a write whose outcome does not depend on what was read.
const ATTEMPTS: usize = 3;

pub fn parse_preset(value: &str) -> Result<PermissionPreset> {
    PRESETS
        .into_iter()
        .find(|preset| name(*preset) == value)
        .ok_or_else(|| {
            "Unknown permission preset; use read-only, workspace-write or full-access".into()
        })
}

pub fn name(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "read-only",
        PermissionPreset::WorkspaceWrite => "workspace-write",
        PermissionPreset::FullAccess => "full-access",
    }
}

/// What Permission's preset step decides, by risk category.
pub fn summary(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => {
            "reads run; writes, commands, network and unclassified tools are denied"
        }
        PermissionPreset::WorkspaceWrite => {
            "reads and file writes run; commands, network and unclassified tools ask"
        }
        PermissionPreset::FullAccess => "every tool call runs without asking",
    }
}

/// The dispatched policy of `main` and the version a change must expect.
#[derive(Deserialize)]
pub struct State {
    pub version: u64,
    pub policy: PermissionPolicy,
}

pub async fn read(client: &mut Client, session_id: &str) -> Result<State> {
    client
        .typed(
            "session.permission.get",
            json!({"sessionId":session_id,"branchId":"main"}),
        )
        .await
}

/// Replaces only the preset; the tool lists that were read are kept.
async fn update(
    client: &mut Client,
    session_id: &str,
    state: &State,
    preset: PermissionPreset,
) -> Result<u64> {
    #[derive(Deserialize)]
    struct Updated {
        version: u64,
    }
    let policy = PermissionPolicy {
        preset,
        ..state.policy.clone()
    };
    let updated: Updated = client
        .typed(
            "session.permission.update",
            json!({"sessionId":session_id,"branchId":"main","expectedVersion":state.version,"policy":policy}),
        )
        .await?;
    Ok(updated.version)
}

/// What [`switch`] did.
pub enum Switch {
    /// `main` already had the preset; nothing was written.
    Unchanged,
    /// `main` has the preset from this version on.
    Switched(u64),
    /// `main` changed after the state the user saw; nothing was written.
    /// Its state now.
    Conflict(State),
}

/// Switches `main` from `seen`, the state the user decided on, so a version
/// conflict is reported with the state read after it, never retried.
pub async fn switch(
    client: &mut Client,
    session_id: &str,
    seen: &State,
    preset: PermissionPreset,
) -> Result<Switch> {
    if seen.policy.preset == preset {
        return Ok(Switch::Unchanged);
    }
    match update(client, session_id, seen, preset).await {
        Ok(version) => Ok(Switch::Switched(version)),
        Err(error) if client::app_error(&error) == Some("permission_conflict") => {
            Ok(Switch::Conflict(read(client, session_id).await?))
        }
        Err(error) => Err(error),
    }
}

/// Selects `preset` for `main` whatever it was before, as a startup choice
/// does: the preset does not depend on the read, so a version conflict reads
/// again and retries.
pub async fn select(client: &mut Client, session_id: &str, preset: PermissionPreset) -> Result<()> {
    let mut state = read(client, session_id).await?;
    for _ in 0..ATTEMPTS {
        match switch(client, session_id, &state, preset).await? {
            Switch::Conflict(current) => state = current,
            Switch::Unchanged | Switch::Switched(_) => return Ok(()),
        }
    }
    Err("The Session's permission preset kept changing; try again".into())
}

/// The preset a `permission/changed` event in its wire form gives, with the
/// event's sequence.
pub fn changed(event: &Value) -> Option<(u64, PermissionPreset)> {
    if event["type"] != "permission" || event["name"] != "changed" {
        return None;
    }
    let data = PermissionChangedData::deserialize(&event["data"]).ok()?;
    Some((event["sequence"].as_u64()?, data.policy.preset))
}

/// The user's command rules and the Settings revision a write must expect.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rules {
    pub revision: String,
    pub command_rules: Vec<CommandRule>,
}

pub async fn rules(client: &mut Client) -> Result<Rules> {
    client.typed("permission.rules", json!({})).await
}

/// A request for the caller to send on its App Server connection.
pub struct Request {
    pub method: &'static str,
    pub params: Value,
}

fn read_rules() -> Request {
    Request {
        method: "permission.rules",
        params: json!({}),
    }
}

fn set_rules(revision: &str, rules: &[CommandRule]) -> Request {
    Request {
        method: "permission.set_rules",
        params: json!({"expectedRevision":revision,"commandRules":rules}),
    }
}

fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|_| format!("Invalid {method} response").into())
}

fn written(value: Value) -> Result<String> {
    #[derive(Deserialize)]
    struct Revision {
        revision: String,
    }
    Ok(decode::<Revision>(value, "permission.set_rules")?.revision)
}

pub fn decision_name(decision: CommandRuleDecision) -> &'static str {
    match decision {
        CommandRuleDecision::Allow => "allow",
        CommandRuleDecision::Deny => "deny",
    }
}

pub fn match_name(matching: CommandMatch) -> &'static str {
    match matching {
        CommandMatch::Exact => "exact",
        CommandMatch::Prefix => "prefix",
    }
}

pub fn describe(rule: &CommandRule) -> String {
    format!(
        "{} {} `{}`",
        decision_name(rule.decision),
        match_name(rule.matching),
        rule.command
    )
}

/// A rule for `command`, which must be a simple command, as Permission
/// matches only those.
pub fn rule(
    command: &str,
    matching: CommandMatch,
    decision: CommandRuleDecision,
) -> Result<CommandRule> {
    let words: Vec<_> = command.split_whitespace().collect();
    if words.is_empty() || command.contains(COMMAND_SEPARATORS) {
        return Err("A command rule needs a simple command without ; & | ` ( ) < > or $".into());
    }
    Ok(CommandRule {
        command: words.join(" "),
        matching,
        decision,
    })
}

/// The list with `rule` appended, or `None` when an equal rule exists.
/// Rules compare word by word, as Permission matches them.
fn with_rule(rules: &[CommandRule], rule: &CommandRule) -> Option<Vec<CommandRule>> {
    let present = rules.iter().any(|existing| {
        existing.decision == rule.decision
            && existing.matching == rule.matching
            && existing
                .command
                .split_whitespace()
                .eq(rule.command.split_whitespace())
    });
    (!present).then(|| {
        let mut rules = rules.to_vec();
        rules.push(rule.clone());
        rules
    })
}

/// What adding a rule did.
pub enum Added {
    /// Written; the list stored now.
    Added(Rules),
    /// An equal rule was already in the list; nothing was written.
    Present,
}

/// Where [`AddRule`] stands after a reply.
pub enum Step {
    /// Send this request and pass its reply to [`AddRule::reply`].
    Send(Request),
    Done(Result<Added>),
}

/// Adding a rule as requests and replies, for a caller that multiplexes its
/// connection; [`add_rule`] runs it call by call. The rule does not depend
/// on the others, so a revision conflict reads again and retries.
pub struct AddRule {
    rule: CommandRule,
    attempts: usize,
    /// The list sent with `permission.set_rules`, while that is outstanding.
    writing: Option<Vec<CommandRule>>,
}

impl AddRule {
    /// The rule to add and the first request: `permission.rules`.
    pub fn start(rule: CommandRule) -> (Self, Request) {
        let add = Self {
            rule,
            attempts: 1,
            writing: None,
        };
        (add, read_rules())
    }

    pub fn rule(&self) -> &CommandRule {
        &self.rule
    }

    /// The next step, given the reply to the request sent last.
    pub fn reply(&mut self, reply: Result<Value>) -> Step {
        let Some(sent) = self.writing.take() else {
            let current = match reply.and_then(|value| decode::<Rules>(value, "permission.rules")) {
                Ok(current) => current,
                Err(error) => return Step::Done(Err(error)),
            };
            let Some(rules) = with_rule(&current.command_rules, &self.rule) else {
                return Step::Done(Ok(Added::Present));
            };
            let request = set_rules(&current.revision, &rules);
            self.writing = Some(rules);
            return Step::Send(request);
        };
        match reply {
            Ok(value) => Step::Done(written(value).map(|revision| {
                Added::Added(Rules {
                    revision,
                    command_rules: sent,
                })
            })),
            Err(error) if client::app_error(&error) == Some("permission_rules_conflict") => {
                if self.attempts == ATTEMPTS {
                    return Step::Done(Err("Command rules kept changing; try again".into()));
                }
                self.attempts += 1;
                Step::Send(read_rules())
            }
            Err(error) => Step::Done(Err(error)),
        }
    }
}

/// Adds `rule` to the user's command rules, for a caller that waits for each
/// reply.
pub async fn add_rule(client: &mut Client, rule: CommandRule) -> Result<Added> {
    let (mut add, mut request) = AddRule::start(rule);
    loop {
        let reply = client.rpc(request.method, request.params).await;
        match add.reply(reply) {
            Step::Send(next) => request = next,
            Step::Done(added) => return added,
        }
    }
}

/// What removing a rule did.
pub enum Removed {
    /// `rule` is gone; `rules` is the list stored now.
    Removed { rule: CommandRule, rules: Rules },
    /// The rules changed after the list the user saw; nothing was written.
    /// The list now.
    Conflict(Rules),
}

/// Removes the rule at `index` of `seen`, the list the user chose from, so a
/// revision conflict is reported with the list read after it, never retried.
pub async fn remove_rule(client: &mut Client, seen: &Rules, index: usize) -> Result<Removed> {
    if index >= seen.command_rules.len() {
        return Err("No such rule in the list".into());
    }
    let mut next = seen.command_rules.clone();
    let rule = next.remove(index);
    let request = set_rules(&seen.revision, &next);
    match client.rpc(request.method, request.params).await {
        Ok(value) => Ok(Removed::Removed {
            rule,
            rules: Rules {
                revision: written(value)?,
                command_rules: next,
            },
        }),
        Err(error) if client::app_error(&error) == Some("permission_rules_conflict") => {
            Ok(Removed::Conflict(rules(client).await?))
        }
        Err(error) => Err(error),
    }
}

/// The command an "always" answer may turn into an allow rule: a simple
/// command of `exec` or `execute`, the tools F2 names. The request carries no
/// risk category; a rule for another tool would never match.
pub fn always_command(call: &ApprovalCallData) -> Option<&str> {
    if !matches!(call.name.as_str(), "exec" | "execute") {
        return None;
    }
    let Some(Value::String(command)) = call.input.get("command") else {
        return None;
    };
    (!command.contains(COMMAND_SEPARATORS) && command.split_whitespace().next().is_some())
        .then_some(command.as_str())
}

/// The allow rule an "always" answer proposes for `command`: `typed` empty
/// keeps the exact command, its leading words make a prefix rule, anything
/// else proposes none.
pub fn proposed(command: &str, typed: &str) -> Option<CommandRule> {
    let words: Vec<_> = command.split_whitespace().collect();
    let chosen: Vec<_> = typed.split_whitespace().collect();
    let (matching, chosen) = if chosen.is_empty() || chosen == words {
        (CommandMatch::Exact, words)
    } else if words.starts_with(&chosen) {
        (CommandMatch::Prefix, chosen)
    } else {
        return None;
    };
    Some(CommandRule {
        command: chosen.join(" "),
        matching,
        decision: CommandRuleDecision::Allow,
    })
}

/// The answer that allows an approval once its rule is in place; a failed
/// rule write leaves the approval open for a once or deny answer.
pub fn allow(session_id: &str, branch_id: &str, request_id: &str) -> Request {
    Request {
        method: "approval.respond",
        params: json!({"sessionId":session_id,"branchId":branch_id,"requestId":request_id,"decision":"allow"}),
    }
}
