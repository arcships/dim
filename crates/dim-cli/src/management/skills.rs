//! The Skills area: the catalog and each entry's enablement
//! (`skills.catalog`, `skills.set_enabled`, `skills.refresh`).
//!
//! Skills reports the root an entry was found in as its `source`; the panel
//! groups by it and never reads the id, which names an absolute project path.

use serde::Deserialize;
use serde_json::json;

use super::Groups;
use crate::client::Client;
use crate::model::session::{
    ActionKind, EntryState, ManagementAction, ManagementEntry, ManagementView, RemoveVerb,
};
use crate::ui::i18n::t;

/// One entry of `skills.catalog`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    /// `user`, `project` or `plugin:<plugin id>`. A daemon older than this
    /// fact reports none, and the client never derives one.
    pub source: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub file_path: String,
    pub enabled: bool,
    pub shadowed_by: Option<String>,
}

/// A Skill the catalog could not use.
#[derive(Debug, Deserialize)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub entries: Vec<Entry>,
    pub issues: Vec<Issue>,
}

/// What a Skills action came to.
pub enum Outcome {
    /// The area as its owner reports it after the action.
    Catalog(Catalog),
    /// The action was applied and the read that follows failed.
    ReadFailed(String),
    /// Nothing was applied, and this is why.
    Refused(String),
}

/// Performs `action`, then reads the area back as its owner reports it.
///
/// The action is applied once the method behind it returns: the owner
/// declares every Skills method except `skills.catalog` mutating, so a
/// `skills.refresh` that succeeded rescanned the roots even when the read
/// after it fails. A kind this area has no method for is refused by
/// `management::supported` before the adapter runs.
pub async fn apply(client: &mut Client, action: &ManagementAction, cwd: &str) -> Outcome {
    let params = json!({"cwd": cwd});
    let applied = match action {
        // Both are reads: `skills.catalog` is the only method they call.
        ManagementAction::List | ManagementAction::Inspect { .. } => false,
        ManagementAction::Reload => {
            if let Err(error) = client.rpc("skills.refresh", params.clone()).await {
                return Outcome::Refused(error.to_string());
            }
            true
        }
        ManagementAction::Toggle { id, enabled } => {
            let mut params = params;
            params["id"] = json!(id);
            params["enabled"] = json!(enabled);
            if let Err(error) = client.rpc("skills.set_enabled", params).await {
                return Outcome::Refused(error.to_string());
            }
            true
        }
        // The kinds this area has no method for: `management::supported`
        // refuses them before the adapter runs, so nothing applied.
        ManagementAction::Save { .. }
        | ManagementAction::Add { .. }
        | ManagementAction::Remove { .. }
        | ManagementAction::Run { .. } => false,
    };
    match catalog(client, cwd).await {
        Ok(catalog) => Outcome::Catalog(catalog),
        Err(error) if applied => Outcome::ReadFailed(error),
        Err(error) => Outcome::Refused(error),
    }
}

/// The catalog as its owner reports it.
pub async fn catalog(client: &mut Client, cwd: &str) -> Result<Catalog, String> {
    client
        .typed("skills.catalog", json!({"cwd": cwd}))
        .await
        .map_err(|error| error.to_string())
}

/// One source's entries under its heading, in the order the owner reports
/// them; `actions` are the kinds the daemon advertises for this area. An
/// entry whose owner reports no source goes under the area's own heading.
pub(super) fn view(catalog: &Catalog, actions: &[ActionKind]) -> ManagementView {
    let mut groups = Groups::default();
    for entry in &catalog.entries {
        let mut states = Vec::new();
        if let Some(by) = &entry.shadowed_by {
            states.push(EntryState::Shadowed { by: by.clone() });
        }
        groups.push(
            entry.source.as_deref().unwrap_or(t("skills")),
            ManagementEntry {
                id: entry.id.clone(),
                label: entry.name.clone(),
                detail: [entry.description.clone(), entry.file_path.clone()].to_vec(),
                enabled: Some(entry.enabled),
                states,
                actions: super::entry_kinds(actions),
                remove_verb: RemoveVerb::Remove,
                document: None,
                run: None,
            },
        );
    }
    ManagementView {
        title: None,
        revision: None,
        document: None,
        create: None,
        actions: super::area_kinds(actions),
        groups: groups.into_vec(),
        issues: catalog
            .issues
            .iter()
            .map(|issue| format!("{}: {}", issue.path, issue.message))
            .collect(),
    }
}
