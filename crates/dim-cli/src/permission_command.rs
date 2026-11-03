//! `/permissions` at the line-mode prompt: permission presets and command rules.
use crate::{
    Result,
    client::Client,
    display::Display,
    input::Input,
    permission::{self, Added, PRESET_METHODS, RULE_METHODS, Removed, Rules, State, Switch},
    session::{Interrupts, settle},
};
use dim_agent_types::PermissionPreset;

const USAGE: &str = "Usage: /permissions [PRESET | allow|deny exact|prefix COMMAND | remove N]";

/// What the prompt says when Ctrl+C arrives while a form is in flight.
const NOTICE: &str = "Waiting for the permission result before accepting more input.";

fn print_state(display: &mut Display, state: &State) -> Result<()> {
    display.record(&format!(
        "permission: {}",
        permission::name(state.policy.preset)
    ))?;
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
            match settle(
                interrupts,
                NOTICE,
                permission::remove_rule(client, seen, index - 1),
            )
            .await?
            {
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
        [preset] if !matches!(preset, "remove" | "allow" | "deny") => {
            let preset = permission::parse_preset(preset)?;
            switch(client, input, interrupts, session_id, preset, display).await
        }
        _ => {
            let Some((decision, matching, command)) = permission::parse_rule_words(&words[1..])
            else {
                return Err(USAGE.into());
            };
            require_rules(client)?;
            let rule = permission::rule(&command, matching, decision)?;
            let described = permission::describe(&rule);
            match settle(interrupts, NOTICE, permission::add_rule(client, rule)).await? {
                Added::Added(rules) => {
                    display.record(&format!("Rule added: {described}"))?;
                    retain(display, listing, rules)
                }
                Added::Present => display.record(&format!("Rule already present: {described}")),
            }
        }
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
        let state = settle(interrupts, NOTICE, permission::read(client, session_id)).await?;
        print_state(display, &state)?;
    } else {
        display.record("Permission presets: this daemon does not provide them")?;
    }
    if client.supports(&RULE_METHODS) {
        let current = settle(interrupts, NOTICE, permission::rules(client)).await?;
        retain(display, listing, current)?;
    } else {
        display.record("Command rules: this daemon does not provide them")?;
    }
    display.record("Process isolation is configured in /settings > Process isolation.")?;
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
    let mut seen = settle(interrupts, NOTICE, permission::read(client, session_id)).await?;
    let name = permission::name(preset);
    if seen.policy.preset == preset {
        display.record(&format!("Preset (main branch) is already {name}"))?;
        return print_state(display, &seen);
    }
    if preset == PermissionPreset::FullAccess {
        display.record("Full access removes path limits and skips tool questions. Explicit denies and process isolation still apply.")?;
        input.prompt("Switch the main branch to full-access? [y/N] ")?;
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
    match settle(
        interrupts,
        NOTICE,
        permission::switch(client, session_id, &seen, preset),
    )
    .await?
    {
        Switch::Unchanged => display.record(&format!("Preset (main branch) is already {name}"))?,
        Switch::Switched(version) => {
            display.permission_seen = display.permission_seen.max(version);
            display.record(&format!(
                "Preset (main branch): {name}. Child branches and members created later from main inherit it; existing branches and members keep their policies."
            ))?;
            seen.policy.preset = preset;
        }
        Switch::Conflict(current) => {
            display.record(&format!(
                "The preset changed elsewhere and is now {}; nothing was changed. Run the command again to switch.",
                permission::name(current.policy.preset)
            ))?;
            seen = current;
        }
    }
    print_state(display, &seen)
}
