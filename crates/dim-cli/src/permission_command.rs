//! `/approvals` (alias `/permissions`) at the line-mode prompt: shows and
//! switches the `main` preset, and lists, adds and removes command rules,
//! through the operations in [`crate::permission`].
use crate::{
    Result,
    client::Client,
    display::Display,
    input::Input,
    permission::{
        self, Added, PRESET_METHODS, PRESETS, RULE_METHODS, Removed, Rules, State, Switch,
    },
};
use dim_agent_types::{CommandMatch, CommandRuleDecision, PermissionPreset};
use std::future::Future;

#[cfg(unix)]
pub type Interrupts = tokio::signal::unix::Signal;
#[cfg(windows)]
pub type Interrupts = tokio::signal::windows::CtrlC;

const USAGE: &str = "Usage: /approvals [PRESET | allow|deny exact|prefix COMMAND | remove N]";

/// Waits for an operation; interrupts are reported and consumed until it
/// settles.
async fn settle<T>(interrupts: &mut Interrupts, operation: impl Future<Output = T>) -> T {
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            _ = interrupts.recv() => {
                println!("\nWaiting for the permission result before accepting more input.");
            }
            result = &mut operation => break result,
        }
    }
}

fn print_state(display: &mut Display, state: &State) -> Result<()> {
    let current = state.policy.preset;
    display.record(&format!(
        "Preset (main branch): {}",
        permission::name(current)
    ))?;
    for preset in PRESETS {
        display.record(&format!(
            "{} {:<16} {}",
            if preset == current { "*" } else { " " },
            permission::name(preset),
            permission::summary(preset)
        ))?;
    }
    for (label, tools) in [
        ("allowed", &state.policy.allowed_tools),
        ("disallowed", &state.policy.disallowed_tools),
    ] {
        if !tools.is_empty() {
            display.record(&format!("  {label} tools: {}", tools.join(", ")))?;
        }
    }
    Ok(())
}

/// Prints `rules` numbered and keeps them as the listing `remove N` numbers
/// from. Nothing else sets the listing, so a number always refers to the list
/// printed last; a list that is not printed is not kept.
fn retain(display: &mut Display, listing: &mut Option<Rules>, rules: Rules) -> Result<()> {
    *listing = None;
    if rules.command_rules.is_empty() {
        display.record("Command rules: none")?;
    } else {
        display.record("Command rules (user scope, every Session):")?;
        for (index, rule) in rules.command_rules.iter().enumerate() {
            display.record(&format!(
                "  {}  {:<5}  {:<6}  {}",
                index + 1,
                permission::decision_name(rule.decision),
                permission::match_name(rule.matching),
                rule.command
            ))?;
        }
    }
    *listing = Some(rules);
    Ok(())
}

fn require_rules(client: &Client) -> Result<()> {
    if client.supports(&RULE_METHODS) {
        Ok(())
    } else {
        Err("This daemon does not provide command rules".into())
    }
}

/// Runs `text`, an `/approvals` command line. `listing` is the rule list
/// printed last, which `remove N` numbers from; its revision makes the write
/// fail if the rules changed since.
pub async fn command(
    client: &mut Client,
    input: &mut Input,
    interrupts: &mut Interrupts,
    session_id: &str,
    text: &str,
    display: &mut Display,
    listing: &mut Option<Rules>,
) -> Result<()> {
    let words: Vec<_> = text.split_whitespace().collect();
    match words[1..] {
        [] => show(client, interrupts, session_id, display, listing).await,
        ["remove", number] => {
            require_rules(client)?;
            let Some(seen) = listing.as_ref() else {
                return Err("Run /approvals to list the rules first".into());
            };
            let index = number
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=seen.command_rules.len()).contains(index))
                .ok_or("No such rule in the last /approvals listing")?;
            match settle(interrupts, permission::remove_rule(client, seen, index - 1)).await? {
                Removed::Removed { rule, rules } => {
                    display.record(&format!("Rule removed: {}", permission::describe(&rule)))?;
                    retain(display, listing, rules)
                }
                Removed::Conflict(current) => {
                    display.record(
                        "Command rules changed since they were listed; nothing was removed.",
                    )?;
                    retain(display, listing, current)
                }
            }
        }
        [
            decision @ ("allow" | "deny"),
            matching @ ("exact" | "prefix"),
            ref command @ ..,
        ] if !command.is_empty() => {
            require_rules(client)?;
            let rule = permission::rule(
                &command.join(" "),
                if matching == "exact" {
                    CommandMatch::Exact
                } else {
                    CommandMatch::Prefix
                },
                if decision == "allow" {
                    CommandRuleDecision::Allow
                } else {
                    CommandRuleDecision::Deny
                },
            )?;
            let described = permission::describe(&rule);
            match settle(interrupts, permission::add_rule(client, rule)).await? {
                Added::Added(rules) => {
                    display.record(&format!("Rule added: {described}"))?;
                    retain(display, listing, rules)
                }
                // Nothing was written: the listing printed last stays.
                Added::Present => display.record(&format!("Rule already present: {described}")),
            }
        }
        [preset] if !matches!(preset, "remove" | "allow" | "deny") => {
            let preset = permission::parse_preset(preset)?;
            switch(client, input, interrupts, session_id, preset, display).await
        }
        _ => Err(USAGE.into()),
    }
}

async fn show(
    client: &mut Client,
    interrupts: &mut Interrupts,
    session_id: &str,
    display: &mut Display,
    listing: &mut Option<Rules>,
) -> Result<()> {
    if client.supports(&PRESET_METHODS) {
        let state = settle(interrupts, permission::read(client, session_id)).await?;
        print_state(display, &state)?;
    } else {
        display.record("Permission presets: this daemon does not provide them")?;
    }
    if client.supports(&RULE_METHODS) {
        let current = settle(interrupts, permission::rules(client)).await?;
        retain(display, listing, current)?;
    } else {
        display.record("Command rules: this daemon does not provide them")?;
    }
    display.record(USAGE)
}

/// Switches `main` from the preset the user saw, after a confirmation for
/// full access.
async fn switch(
    client: &mut Client,
    input: &mut Input,
    interrupts: &mut Interrupts,
    session_id: &str,
    preset: PermissionPreset,
    display: &mut Display,
) -> Result<()> {
    if !client.supports(&PRESET_METHODS) {
        return Err("This daemon cannot change the permission preset".into());
    }
    let seen = settle(interrupts, permission::read(client, session_id)).await?;
    let name = permission::name(preset);
    if seen.policy.preset == preset {
        return display.record(&format!("Preset (main branch) is already {name}"));
    }
    if preset == PermissionPreset::FullAccess {
        display.record("Full access runs every tool call, including commands, without asking.")?;
        input.prompt("Switch the main branch to full-access? [y/N] ")?;
        // Only y/yes confirms; Ctrl+C and EOF decline.
        let line = tokio::select! {
            biased;
            _ = interrupts.recv() => {
                println!();
                None
            }
            line = input.line() => line?,
        };
        if !line
            .is_some_and(|line| matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
        {
            return display.record(&format!(
                "Preset unchanged: {}",
                permission::name(seen.policy.preset)
            ));
        }
    }
    match settle(interrupts, permission::switch(client, session_id, &seen, preset)).await? {
        Switch::Unchanged => display.record(&format!("Preset (main branch) is already {name}")),
        Switch::Switched(version) => {
            // The live `permission/changed` for this write is already reported.
            display.permission_seen = display.permission_seen.max(version);
            display.record(&format!(
                "Preset (main branch): {name}. Child branches created later inherit it; agent members keep the Session's starting preset."
            ))
        }
        Switch::Conflict(current) => display.record(&format!(
            "The preset changed elsewhere and is now {}; nothing was changed. Run the command again to switch.",
            permission::name(current.policy.preset)
        )),
    }
}
