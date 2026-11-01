//! The `hooks` commands of the line client: the effective hook catalog and
//! this workspace's enablement (Hooks design A.2, A.7).
use crate::{Result, args::Options, client::Client, mcp_command::print_report, sanitize::safe};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    projects_revision: String,
    #[serde(default)]
    disable_all_hooks: bool,
    sources: Vec<Source>,
    hooks: Vec<Hook>,
}
#[derive(Deserialize)]
struct Source {
    source: String,
    path: Option<String>,
    enabled: bool,
    state: String,
    message: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hook {
    source: String,
    event: String,
    matcher: Option<String>,
    command: Option<String>,
    timeout: u64,
    on_failure: String,
    r#async: bool,
    state: String,
}

/// `hooks list [--cwd DIR]`: every handler the sources hold, with the state
/// that decides whether it runs.
pub async fn list(client: &mut Client, options: &Options) -> Result<()> {
    let catalog: Catalog = client
        .typed("hooks.catalog", json!({"cwd": options.cwd}))
        .await?;
    let mut rows = Vec::new();
    for hook in &catalog.hooks {
        let enabled = catalog
            .sources
            .iter()
            .find(|source| source.source == hook.source)
            .is_some_and(|source| source.enabled);
        rows.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}s\t{}\t{}\t{}",
            safe(&hook.event),
            safe(hook.matcher.as_deref().unwrap_or("*")),
            safe(&hook.source),
            if enabled { "enabled" } else { "disabled" },
            safe(&hook.state),
            hook.timeout,
            safe(&hook.on_failure),
            if hook.r#async { "async" } else { "sync" },
            safe(hook.command.as_deref().unwrap_or("—")),
        ));
    }
    // The columns describe rows: with no handler, the report says so instead.
    let mut notes = if rows.is_empty() {
        vec!["No hook is configured.".to_owned()]
    } else {
        vec![
            "Columns: event, matcher, source, enablement, state, timeout, on failure, async, command.\nA handler whose state is ok runs at its event once its source is enabled; unsupported and invalid handlers never run.\nProject hooks stay disabled until you run hooks enable.".to_owned(),
        ]
    };
    for source in &catalog.sources {
        let name = match source.path.as_deref() {
            Some(path) => format!("{} hooks ({})", safe(&source.source), safe(path)),
            None => format!("{} hooks", safe(&source.source)),
        };
        if source.state != "ok" {
            notes.push(format!(
                "{name}: {}{}",
                safe(&source.state),
                source
                    .message
                    .as_deref()
                    .map(|message| format!(": {}", safe(message)))
                    .unwrap_or_default()
            ));
        } else if !source.enabled {
            notes.push(format!("{name}: disabled"));
        }
    }
    if catalog.disable_all_hooks {
        notes.push("disableAllHooks in the user document turns every hook off.".to_owned());
    }
    print_report(rows, notes);
    Ok(())
}

/// `hooks enable|disable [--cwd DIR]`: this workspace's `.dimcode/hooks.json`
/// enablement in the user-local record.
pub async fn set_enabled(client: &mut Client, options: &Options, enabled: bool) -> Result<()> {
    let catalog: Catalog = client
        .typed("hooks.catalog", json!({"cwd": options.cwd}))
        .await?;
    let cwd = options.cwd.to_string_lossy().into_owned();
    client
        .rpc(
            "hooks.set_enabled",
            json!({
                "cwd": cwd,
                "enabled": enabled,
                "expectedRevision": catalog.projects_revision,
            }),
        )
        .await?;
    let mut notes = Vec::new();
    if enabled {
        notes.push(
            "It runs this workspace's .dimcode/hooks.json, including later edits to it, in active and new sessions."
                .to_owned(),
        );
    } else {
        notes.push("The file is kept; it runs again after hooks enable.".to_owned());
    }
    print_report(
        vec![format!(
            "{} project hooks for {}.",
            if enabled { "Enabled" } else { "Disabled" },
            safe(&cwd)
        )],
        notes,
    );
    Ok(())
}
