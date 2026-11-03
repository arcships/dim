//! The Hooks area: the user document and the sources that run hooks
//! (`hooks.catalog`, `hooks.save`, `hooks.set_enabled`).
//!
//! The sources are the entries. The user source carries the one document the
//! client may write, so it offers edit and no toggle; a project source is
//! turned on or off for this workspace alone; a plugin's source belongs to
//! its plugin.

use serde::Deserialize;
use serde_json::{Value, json};

use super::Groups;
use crate::client::Client;
use crate::model::session::{
    ActionKind, EntryState, ManagementAction, ManagementEntry, ManagementView, RemoveVerb,
};
use crate::ui::i18n::{t, tf};

/// The project source, the one `hooks.set_enabled` writes.
pub const PROJECT: &str = "project";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub user_revision: String,
    /// The user's document as stored, which `hooks.save` replaces with
    /// `userRevision`. A daemon that reports none offers no edit.
    #[serde(default)]
    pub user_document: Option<Value>,
    pub projects_revision: String,
    #[serde(default)]
    pub disable_all_hooks: bool,
    pub sources: Vec<Source>,
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub source: String,
    pub path: Option<String>,
    pub enabled: bool,
    pub state: String,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hook {
    pub source: String,
    pub event: String,
    pub matcher: Option<String>,
    pub command: Option<String>,
    pub timeout: u64,
    pub on_failure: String,
    pub r#async: bool,
    pub state: String,
}

/// Why a Hooks action did not produce a catalog.
pub enum Failure {
    /// The stored document changed since it was read: nothing was written,
    /// and this is the catalog as it now is, or why the read that reports it
    /// failed.
    Conflict {
        message: String,
        catalog: Result<Box<Catalog>, String>,
    },
    /// The action was applied and the read that follows failed.
    ReadFailed(String),
    /// The daemon or the owner refused it: nothing was written.
    Refused(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, catalog } => match catalog {
                Ok(_) => formatter.write_str(message),
                Err(error) => write!(formatter, "{message} {error}"),
            },
            Self::ReadFailed(message) | Self::Refused(message) => formatter.write_str(message),
        }
    }
}

/// What a write the owner refused over a newer document leaves: nothing was
/// written, and the area is read again so the next write starts from what is
/// stored now. A read that fails is still a conflict, and says why it cannot
/// show the area.
async fn conflict(client: &mut Client, cwd: &str, message: &'static str) -> Failure {
    Failure::Conflict {
        message: t(message).to_owned(),
        catalog: catalog(client, cwd).await.map(Box::new),
    }
}

/// Performs `action`, then reads the area back as its owner reports it.
pub async fn apply(
    client: &mut Client,
    action: &ManagementAction,
    cwd: &str,
) -> Result<Catalog, Failure> {
    let mut applied = false;
    match action {
        ManagementAction::List | ManagementAction::Reload => {}
        ManagementAction::Toggle { id, enabled } => {
            if id != PROJECT {
                return Err(Failure::Refused(format!(
                    "Only this workspace's {PROJECT} hooks can be turned on or off here."
                )));
            }
            let current = catalog(client, cwd).await.map_err(Failure::Refused)?;
            let toggled = client
                .rpc(
                    "hooks.set_enabled",
                    json!({
                        "cwd": cwd,
                        "enabled": enabled,
                        "expectedRevision": current.projects_revision,
                    }),
                )
                .await;
            if let Err(error) = toggled {
                // A conflict is its own case, as for a save: nothing was
                // written, and the area is read again.
                if crate::client::app_error(&error) != Some("hooks_conflict") {
                    return Err(Failure::Refused(error.to_string()));
                }
                return Err(
                    conflict(
                        client,
                        cwd,
                        "This workspace's hook settings changed since they were read; nothing was written.",
                    )
                    .await,
                );
            }
            applied = true;
        }
        ManagementAction::Save {
            expected_revision,
            document,
            ..
        } => {
            let saved = client
                .rpc(
                    "hooks.save",
                    json!({"expectedRevision": expected_revision, "document": document}),
                )
                .await;
            if let Err(error) = saved {
                // A conflict is its own case: the client drops its link to
                // the stale revision, and the area is read again so the next
                // edit starts from the stored document.
                if crate::client::app_error(&error) != Some("hooks_conflict") {
                    return Err(Failure::Refused(error.to_string()));
                }
                return Err(conflict(
                    client,
                    cwd,
                    "The user document changed since it was read; nothing was written.",
                )
                .await);
            }
            applied = true;
        }
        // A kind this area has no method for is refused by
        // `management::supported` before the adapter runs.
        _ => {}
    }
    catalog(client, cwd).await.map_err(|error| {
        if applied {
            Failure::ReadFailed(error)
        } else {
            Failure::Refused(error)
        }
    })
}

/// The catalog as its owner reports it.
pub async fn catalog(client: &mut Client, cwd: &str) -> Result<Catalog, String> {
    client
        .typed("hooks.catalog", json!({"cwd": cwd}))
        .await
        .map_err(|error| error.to_string())
}

/// The sources as entries, with the kinds the daemon can serve.
pub(super) fn view(catalog: &Catalog, available: &[ActionKind]) -> ManagementView {
    let mut groups = Groups::default();
    for source in &catalog.sources {
        let offered: &[ActionKind] = match source.source.as_str() {
            "user" if catalog.user_document.is_some() => &[ActionKind::Edit],
            PROJECT => &[ActionKind::Toggle],
            _ => &[],
        };
        let mut states = Vec::new();
        // A source its owner reports as missing has no document to use, which
        // the detail says; only one it reports as invalid could not be used.
        if source.state == "invalid" {
            states.push(EntryState::Invalid);
        }
        if !matches!(source.source.as_str(), "user" | PROJECT) {
            states.push(EntryState::ReadOnly);
        }
        groups.push(
            t("sources"),
            ManagementEntry {
                id: source.source.clone(),
                label: source.source.clone(),
                detail: detail(source),
                enabled: Some(source.enabled),
                states,
                actions: offered
                    .iter()
                    .filter(|kind| available.contains(kind))
                    .copied()
                    .collect(),
                remove_verb: RemoveVerb::Remove,
                document: None,
                run: None,
            },
        );
    }
    let mut issues = Vec::new();
    if catalog.disable_all_hooks {
        issues.push(t("The user document turns every hook off (disableAllHooks).").to_owned());
    }
    ManagementView {
        title: None,
        revision: Some(catalog.user_revision.clone()),
        document: catalog.user_document.clone(),
        create: None,
        actions: super::area_kinds(available),
        groups: groups.into_vec(),
        issues,
    }
}

/// The line a source's state adds to what it says about itself, in the one
/// wording the panel and `dimcode hooks list` show. A source with a path its
/// owner reported as missing has no hooks file there; an invalid one names
/// what its owner could not use. An `ok` source has no line.
pub fn state_line(source: &Source) -> Option<String> {
    match (source.state.as_str(), source.path.as_deref()) {
        ("ok", _) => None,
        ("missing", Some(_)) => Some(t("There is no hooks file there.").to_owned()),
        (state, _) => Some(match source.message.as_deref() {
            Some(message) => format!("{state}: {message}"),
            None => state.to_owned(),
        }),
    }
}

/// What a source says about itself.
fn detail(source: &Source) -> Vec<String> {
    let mut lines = Vec::new();
    match source.source.as_str() {
        "user" => lines.push(t("The user's own document; always in effect.").to_owned()),
        PROJECT => lines.extend(source.path.iter().cloned()),
        _ => {
            let plugin = source.source.trim_start_matches("plugin:");
            lines.push(tf("Owned by plugin {plugin}.", &[("plugin", plugin)]));
        }
    }
    lines.extend(state_line(source));
    lines
}
