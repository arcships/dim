//! `dimcode plugin …` (PluginManager §8.2): each subcommand is one `plugin.*`
//! management call; the daemon does the work.
use crate::{Result, args::Options, args::Plugin, client::Client, sanitize::safe};
use serde_json::{Value, json};
use std::io::Write;

pub async fn run(client: &mut Client, _: &Options, command: &Plugin) -> Result<i32> {
    let (method, params) = match command {
        Plugin::List => {
            let plugins = client.rpc("plugin.list", json!({})).await?;
            for plugin in plugins.as_array().into_iter().flatten() {
                let mut line = format!(
                    "{}\t{}\t{}",
                    text(&plugin["id"]),
                    text(&plugin["kind"]),
                    text(&plugin["status"])
                );
                if plugin["locked"] == true {
                    line.push_str("\tlocked");
                }
                if let Some(waiting) = plugin["waitingFor"].as_array() {
                    let names: Vec<_> = waiting.iter().map(text).collect();
                    line.push_str(&format!("\twaiting for {}", names.join(", ")));
                }
                if plugin["error"].is_string() {
                    line.push_str(&format!("\t{}", text(&plugin["error"])));
                }
                println!("{line}");
            }
            return Ok(0);
        }
        Plugin::Install {
            source,
            reference,
            id,
        } => {
            let source = if is_git(source) {
                json!({"git": source, "ref": reference})
            } else {
                if reference.is_some() {
                    return Err("--ref applies to a Git URL".into());
                }
                // Relative to the CLI's current directory, like other paths.
                let path = std::fs::canonicalize(std::env::current_dir()?.join(source))
                    .map_err(|error| format!("Cannot read plugin {source}: {error}"))?;
                json!({"path": path})
            };
            ("plugin.install", json!({"source": source, "id": id}))
        }
        Plugin::SetEnabled { id, enabled } => {
            ("plugin.set_enabled", json!({"id": id, "enabled": enabled}))
        }
        Plugin::Reload(id) => ("plugin.reload", json!({"id": id})),
        Plugin::Update { id, confirmed } => {
            // Updating takes the user's confirmation; the daemon only does it.
            if !confirmed && !confirm(&format!("Update {id} to the newest commit of its branch?"))?
            {
                eprintln!("Not updated.");
                return Ok(1);
            }
            ("plugin.update", json!({"id": id}))
        }
        Plugin::Remove(id) => ("plugin.remove", json!({"id": id})),
    };
    let result = client.rpc(method, params).await?;
    report(&result);
    Ok(0)
}

/// A Git URL rather than a local path.
fn is_git(source: &str) -> bool {
    source.contains("://") || source.starts_with("git@") || source.ends_with(".git")
}

fn text(value: &Value) -> String {
    safe(value.as_str().unwrap_or_default()).replace(['\n', '\t'], " ")
}

fn report(result: &Value) {
    let mut line = match result["id"].as_str() {
        Some(id) => format!("{}: {}", safe(id), text(&result["status"])),
        None => text(&result["status"]),
    };
    if let Some(commit) = result["commit"].as_str() {
        line.push_str(&format!(" at {}", safe(commit)));
    }
    if result["error"].is_string() {
        line.push_str(&format!(" ({})", text(&result["error"])));
    }
    println!("{line}");
    if result["restartRequired"] == true {
        eprintln!(
            "Some of its names take effect after the daemon restarts: run `dimcode daemon stop`, then start dimcode again."
        );
    }
    if let Some(error) = result["cleanupError"].as_str() {
        eprintln!("Its files could not be deleted: {}", safe(error));
    }
}

/// Asks on the terminal; anything but y/yes declines, as approval does.
fn confirm(question: &str) -> Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err("Confirm with --yes when not on a terminal".into());
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "yes"))
}
