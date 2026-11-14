//! The models area: one Provider's models (`provider.*`).
//!
//! The Provider is the area's scope, so its id travels with every request.
//! A model is the entry: the owner reports its source, state and actions,
//! and the Provider record version the read reports is the revision every
//! write of the area takes. Every write replies with the Provider catalog,
//! so the adapter reads the area again after one.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Groups;
use crate::client::{Client, app_error};
use crate::model::session::{
    ActionKind, EntryDocument, EntryState, ManagementAction, ManagementDetail, ManagementEntry,
    ManagementView, RemoveVerb,
};
use crate::ui::i18n::{t, tf};
use crate::ui::oauth_view::count;

/// The fields `provider.models.edit` takes, in the template's order.
const EDITABLE: &[&str] = &[
    "displayName",
    "contextWindow",
    "maxOutputTokens",
    "toolCalling",
    "vision",
    "reasoning",
    "modalityCapabilities",
    "endpoint",
    "alias",
];

/// `provider.management`'s reply: the Providers it could read, and the ones
/// it could not, each with the owner's message.
#[derive(Debug, Deserialize)]
pub struct Reply {
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub errors: Vec<RecordError>,
}

/// A Provider record the owner could not read.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordError {
    pub provider_id: String,
    pub message: String,
}

/// One Provider as `provider.management` reports it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub version: Option<u32>,
    /// The operations the owner offers for this Provider.
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    connection: Option<Connection>,
    /// `None` for an account the owner has no stored catalog for.
    #[serde(default)]
    pub models: Option<Vec<Model>>,
}

impl Provider {
    /// The model with this id, as the read reports it.
    pub fn model(&self, id: &str) -> Option<&Model> {
        self.models
            .as_deref()?
            .iter()
            .find(|model| model.entry.model_id == id)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    driver_kind: String,
}

/// One model of the Provider's catalog.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    #[serde(rename = "model")]
    pub entry: Entry,
    /// `default`, `override` or `user`, as the owner reports it.
    pub source: String,
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    /// The operations the owner offers for this model.
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub endpoint_override: Option<String>,
    /// The model this entry's id stands for, when the Provider says so.
    #[serde(default)]
    pub alias: Option<String>,
}

/// A `ModelEntry`, as the owner reports it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub status: String,
    pub capabilities: dim_agent_types::CatalogCapabilities,
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
    #[serde(default)]
    pub reasoning: Option<dim_agent_types::ModelReasoning>,
    #[serde(default)]
    pub matched_provider_id: Option<String>,
    #[serde(default)]
    pub matched_model_id: Option<String>,
    #[serde(default)]
    pub match_strategy: Option<String>,
    #[serde(default)]
    pub capability_override_fields: Vec<String>,
    #[serde(default)]
    pub identification: Option<crate::wire::Identification>,
    #[serde(default)]
    pub raw: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    #[serde(default)]
    pub input_token_price: Option<f64>,
    #[serde(default)]
    pub output_token_price: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    #[serde(default)]
    pub requests_per_minute: Option<u64>,
    #[serde(default)]
    pub tokens_per_minute: Option<u64>,
}

/// Why a models action did not happen.
pub enum Failure {
    /// The Provider changed since it was read: nothing was written, and this
    /// is the area as it now is, or why the read that classifies it failed.
    Conflict {
        message: String,
        provider: Result<Box<Provider>, String>,
    },
    /// The action was applied and the read that reports the area failed: the
    /// change was made, and this is why the area is not shown.
    ReadFailed { note: Option<String>, error: String },
    /// The daemon or the owner refused it: nothing was written.
    Refused(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, .. }
            | Self::Refused(message)
            | Self::ReadFailed { error: message, .. } => formatter.write_str(message),
        }
    }
}

/// What a models action came to.
pub enum Answer {
    /// The Provider after the action, and what the owner answered when that
    /// is more than the catalog.
    Provider {
        provider: Provider,
        /// The owner's answer when it says more than the view, such as the
        /// name an add made or what a reset did.
        note: Option<String>,
    },
    /// An inspect: one model's descriptor, not a view.
    Detail(ManagementDetail),
}

/// The Provider as `provider.management` reports it.
pub async fn read(client: &mut Client, provider_id: &str) -> Result<Provider, Failure> {
    let reply: Reply = client
        .typed("provider.management", json!({"providerId": provider_id}))
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    if let Some(provider) = reply
        .providers
        .into_iter()
        .find(|provider| provider.provider_id == provider_id)
    {
        return Ok(provider);
    }
    // The owner named a record it could not read: its own message says why,
    // and stands before the not-found fallback.
    match reply
        .errors
        .into_iter()
        .find(|error| error.provider_id == provider_id)
    {
        Some(error) => Err(Failure::Refused(error.message)),
        None => Err(Failure::Refused(t("No provider has that id.").to_owned())),
    }
}

/// Performs `action`, then reads the area back as its owner reports it.
pub async fn apply(
    client: &mut Client,
    provider_id: &str,
    action: &ManagementAction,
) -> Result<Answer, Failure> {
    match action {
        ManagementAction::List => {
            let provider = read(client, provider_id).await?;
            Ok(Answer::Provider {
                provider,
                note: None,
            })
        }
        ManagementAction::Reload => reload(client, provider_id).await,
        ManagementAction::Toggle { id, enabled, .. } => {
            toggle(client, provider_id, id, *enabled).await
        }
        ManagementAction::Save { id, .. } => match id {
            Some(id) => edit(client, provider_id, id, action).await,
            None => Err(Failure::Refused(
                t("A Provider's models have no area document.").to_owned(),
            )),
        },
        ManagementAction::Add { document } => add(client, provider_id, document).await,
        ManagementAction::Remove { id, .. } => remove(client, provider_id, id).await,
        ManagementAction::Inspect { id, .. } => detail(client, provider_id, id).await,
        ManagementAction::Run { .. } => Err(Failure::Refused(
            t("Models have no run operation.").to_owned(),
        )),
    }
}

/// Reload reads the area, and rescans it through `provider.refresh` when the
/// Provider offers that. A refused listing writes nothing, so the stored
/// list stays with the owner's reason as the note.
async fn reload(client: &mut Client, provider_id: &str) -> Result<Answer, Failure> {
    let before = read(client, provider_id).await?;
    let account_changed = before.models.as_deref().is_some_and(|models| {
        models
            .iter()
            .any(|model| model.unavailable_reason.as_deref() == Some("account_changed"))
    });
    if !before.actions.iter().any(|action| action == "refresh")
        || !client.supports(&["provider.refresh"])
    {
        let note = listing_note(&before);
        return Ok(Answer::Provider {
            provider: before,
            note: Some(note),
        });
    }
    let (provider, note) = match client
        .rpc("provider.refresh", json!({"providerId": provider_id}))
        .await
    {
        Ok(_) => {
            // The owner wrote: a read that fails afterwards reports the
            // refresh as applied, with the reason the area is not shown.
            let applied = if account_changed {
                t("The signed-in account changed; the refresh cleared preferences, overrides and disables.").to_owned()
            } else {
                t("Refreshed the models.").to_owned()
            };
            let after = read_after(client, provider_id, Some(applied.clone())).await?;
            let note = if account_changed {
                applied
            } else {
                listing_note(&after)
            };
            (after, note)
        }
        Err(error) => {
            let after = read(client, provider_id).await?;
            let reason = error.to_string();
            (
                after,
                tf(
                    "Could not list models: {error}. Showing the stored list.",
                    &[("error", &reason)],
                ),
            )
        }
    };
    Ok(Answer::Provider {
        provider,
        note: Some(note),
    })
}

/// The listing's own answer: how many models it holds.
fn listing_note(provider: &Provider) -> String {
    match provider.models.as_deref().map_or(0, |models| models.len()) {
        0 => t("The provider listed no models.").to_owned(),
        count => tf("Listed {count} models.", &[("count", &count.to_string())]),
    }
}

/// Enables or disables one model: the set is the prepared one with this
/// model flipped, because the owner takes a complete set.
async fn toggle(
    client: &mut Client,
    provider_id: &str,
    id: &str,
    enabled: bool,
) -> Result<Answer, Failure> {
    let provider = read(client, provider_id).await?;
    let version = version(&provider)?;
    let models = provider.models.as_deref().unwrap_or_default();
    let Some(model) = models.iter().find(|model| model.entry.model_id == id) else {
        return Err(Failure::Refused(t("No model has that id.").to_owned()));
    };
    // The set governs the entries the account can use: the owner keeps an
    // unavailable entry's enablement whatever the set says, so the write is
    // refused before it is sent rather than reported as done.
    if let Some(reason) = &model.unavailable_reason {
        return Err(Failure::Refused(tf(
            "{name} is unavailable ({reason}); its enablement is kept as it is.",
            &[("name", id), ("reason", reason)],
        )));
    }
    let ids: Vec<&str> = models
        .iter()
        .filter(|model| {
            if model.entry.model_id == id {
                enabled
            } else {
                model.enabled
            }
        })
        .map(|model| model.entry.model_id.as_str())
        .collect();
    let written = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId": provider_id, "expectedVersion": version, "modelIds": ids}),
        )
        .await;
    if let Err(error) = written {
        return Err(conflict(client, provider_id, &error, version, ConflictKind::Toggle).await);
    }
    reread(client, provider_id, None).await
}

/// Removes one model: a user model is deleted and an override is reset to
/// its catalog entry, as the owner's own actions say.
async fn remove(client: &mut Client, provider_id: &str, id: &str) -> Result<Answer, Failure> {
    let provider = read(client, provider_id).await?;
    let version = version(&provider)?;
    let Some(model) = provider.model(id) else {
        return Err(Failure::Refused(t("No model has that id.").to_owned()));
    };
    let (method, verb) = if model.actions.iter().any(|action| action == "reset") {
        ("provider.models.reset", RemoveVerb::Reset)
    } else if model.actions.iter().any(|action| action == "delete") {
        ("provider.models.delete", RemoveVerb::Remove)
    } else {
        return Err(Failure::Refused(
            t("This model cannot be removed.").to_owned(),
        ));
    };
    let label = model.entry.display_name.clone();
    let written = client
        .rpc(
            method,
            json!({"providerId": provider_id, "expectedVersion": version, "modelId": id}),
        )
        .await;
    if let Err(error) = written {
        return Err(conflict(client, provider_id, &error, version, ConflictKind::Remove).await);
    }
    // A reset puts the catalog entry back rather than removing anything, so
    // its line says that, naming the entry from the read the write was made
    // at.
    let note = matches!(verb, RemoveVerb::Reset).then(|| {
        tf(
            "Reset {name} to its catalog entry.",
            &[("name", label.as_str())],
        )
    });
    reread(client, provider_id, note).await
}

/// Writes the fields a draft changed, diffed against the template built from
/// the entry the fresh read reports.
async fn edit(
    client: &mut Client,
    provider_id: &str,
    id: &str,
    action: &ManagementAction,
) -> Result<Answer, Failure> {
    let ManagementAction::Save {
        expected_revision,
        document,
        ..
    } = action
    else {
        unreachable!("edit is a save");
    };
    let provider = read(client, provider_id).await?;
    let version = version(&provider)?;
    if version.to_string() != *expected_revision {
        return Err(Failure::Conflict {
            message: conflict_message(ConflictKind::Edit),
            provider: Ok(Box::new(provider)),
        });
    }
    let Some(model) = provider.model(id) else {
        return Err(Failure::Refused(t("No model has that id.").to_owned()));
    };
    let Some(draft) = document.as_object() else {
        return Err(Failure::Refused(
            t("The document is not an object; nothing was written.").to_owned(),
        ));
    };
    let stored = edit_template(&provider, model);
    let mut edit = Map::new();
    for key in EDITABLE {
        if let Some(value) = draft.get(*key)
            && stored.get(*key) != Some(value)
        {
            edit.insert((*key).to_owned(), value.clone());
        }
    }
    if edit.is_empty() {
        return Err(Failure::Refused(
            t("The document changes no editable field; nothing was written.").to_owned(),
        ));
    }
    let written = client
        .rpc(
            "provider.models.edit",
            json!({"providerId": provider_id, "expectedVersion": version,
                "modelId": id, "edit": edit}),
        )
        .await;
    if let Err(error) = written {
        return Err(conflict(client, provider_id, &error, version, ConflictKind::Edit).await);
    }
    reread(client, provider_id, None).await
}

/// Creates one model at the version read immediately before the write.
async fn add(client: &mut Client, provider_id: &str, document: &Value) -> Result<Answer, Failure> {
    let provider = read(client, provider_id).await?;
    let version = version(&provider)?;
    let name = document
        .get("displayName")
        .and_then(Value::as_str)
        .or_else(|| document.get("modelId").and_then(Value::as_str))
        .ok_or_else(|| Failure::Refused(t("The model document names no model.").to_owned()))?
        .to_owned();
    let written = client
        .rpc(
            "provider.models.create",
            json!({"providerId": provider_id, "expectedVersion": version, "model": document}),
        )
        .await;
    if let Err(error) = written {
        return Err(conflict(client, provider_id, &error, version, ConflictKind::Add).await);
    }
    let note = tf("Added {name}.", &[("name", &name)]);
    reread(client, provider_id, Some(note)).await
}

/// One model's descriptor, in place of the list.
async fn detail(client: &mut Client, provider_id: &str, id: &str) -> Result<Answer, Failure> {
    let provider = read(client, provider_id).await?;
    let Some(model) = provider.model(id) else {
        return Err(Failure::Refused(t("No model has that id.").to_owned()));
    };
    Ok(Answer::Detail(descriptor(model)))
}

/// Reads the area after an action the owner accepted: a failed read reports
/// the action as applied with the action's note, not as a refusal.
async fn read_after(
    client: &mut Client,
    provider_id: &str,
    note: Option<String>,
) -> Result<Provider, Failure> {
    match read(client, provider_id).await {
        Ok(provider) => Ok(provider),
        // The action was applied: only the read that reports the area
        // failed, so the change stands and the owner's answer is kept.
        Err(Failure::Refused(error)) => Err(Failure::ReadFailed { note, error }),
        Err(error) => Err(error),
    }
}

/// The area after a write, with the note the action left.
async fn reread(
    client: &mut Client,
    provider_id: &str,
    note: Option<String>,
) -> Result<Answer, Failure> {
    let provider = read_after(client, provider_id, note.clone()).await?;
    Ok(Answer::Provider { provider, note })
}

fn version(provider: &Provider) -> Result<u32, Failure> {
    provider.version.ok_or_else(|| {
        Failure::Refused(t("The provider has no model catalog to write.").to_owned())
    })
}

/// Whether a conflict concerns an edit, an add, a remove or a toggle.
#[derive(Clone, Copy)]
enum ConflictKind {
    Edit,
    Add,
    Remove,
    Toggle,
}

/// The area after a refusal, when the owner refused because the Provider
/// changed. A conflict whose fresh read reports the same version was the
/// owner's own refusal, such as a duplicate model id.
async fn conflict(
    client: &mut Client,
    provider_id: &str,
    error: &crate::Error,
    version: u32,
    kind: ConflictKind,
) -> Failure {
    if app_error(error) != Some("provider_conflict") {
        return Failure::Refused(error.to_string());
    }
    let message = conflict_message(kind);
    match read(client, provider_id).await {
        Ok(provider) if provider.version == Some(version) => Failure::Refused(error.to_string()),
        Ok(provider) => Failure::Conflict {
            message,
            provider: Ok(Box::new(provider)),
        },
        // The conflict stands: the owner kept the stored document because it
        // changed, and the read that would report it now failed.
        Err(Failure::Refused(read_error)) => Failure::Conflict {
            message,
            provider: Err(read_error),
        },
        Err(error) => error,
    }
}

fn conflict_message(kind: ConflictKind) -> String {
    t(match kind {
        ConflictKind::Edit | ConflictKind::Add | ConflictKind::Toggle => {
            "The models changed since they were read; nothing was written."
        }
        ConflictKind::Remove => "The models changed since they were read; nothing was removed.",
    })
    .to_owned()
}

/// The Provider as entries: `default` models under `catalog`, overrides
/// under `overrides` and user models under `added`.
pub fn view(provider: &Provider, available: &[ActionKind]) -> ManagementView {
    let mut actions = super::area_kinds(available);
    if !provider
        .actions
        .iter()
        .any(|action| action == "models.create")
    {
        actions.retain(|kind| *kind != ActionKind::Add);
    }
    let mut groups = Groups::default();
    let revision = provider.version;
    if let Some(models) = &provider.models {
        for (heading, source) in [
            ("catalog", "default"),
            ("overrides", "override"),
            ("added", "user"),
        ] {
            for model in models.iter().filter(|model| model.source == source) {
                groups.push(t(heading), entry(provider, model, revision, available));
            }
        }
        for model in models
            .iter()
            .filter(|model| !matches!(model.source.as_str(), "default" | "override" | "user"))
        {
            groups.push(&model.source, entry(provider, model, revision, available));
        }
    }
    ManagementView {
        title: Some(tf("Models · {name}", &[("name", &provider.display_name)])),
        revision: None,
        document: None,
        create: actions.contains(&ActionKind::Add).then(add_template),
        actions,
        groups: groups.into_vec(),
        issues: match &provider.models {
            Some(_) => Vec::new(),
            None => vec![t("The provider has no catalog yet.").to_owned()],
        },
    }
}

/// One model as an entry.
fn entry(
    provider: &Provider,
    model: &Model,
    version: Option<u32>,
    available: &[ActionKind],
) -> ManagementEntry {
    let mut kinds = Vec::new();
    let mut remove_verb = RemoveVerb::default();
    for action in &model.actions {
        match action.as_str() {
            "set_enabled" => kinds.push(ActionKind::Toggle),
            "edit" => kinds.push(ActionKind::Edit),
            "reset" => {
                kinds.push(ActionKind::Remove);
                remove_verb = RemoveVerb::Reset;
            }
            "delete" => {
                kinds.push(ActionKind::Remove);
                remove_verb = RemoveVerb::Remove;
            }
            _ => {}
        }
    }
    kinds.push(ActionKind::Inspect);
    kinds.dedup();
    kinds.retain(|kind| available.contains(kind));
    let mut states = Vec::new();
    if model.unavailable_reason.is_some() {
        states.push(EntryState::Unavailable);
    }
    ManagementEntry {
        id: model.entry.model_id.clone(),
        source: None,
        label: model.entry.display_name.clone(),
        detail: detail_lines(model),
        // Every model has an on/off state, including an unavailable static
        // model, whose enablement the owner's set keeps.
        enabled: Some(model.enabled),
        states,
        actions: kinds,
        remove_verb,
        run: None,
        document: version.map(|version| EntryDocument {
            revision: version.to_string(),
            document: edit_template(provider, model),
        }),
    }
}

/// What a row's detail says: the id, the limits, the endpoint, and the
/// owner's reason when the model cannot be used.
fn detail_lines(model: &Model) -> Vec<String> {
    let mut lines = vec![model.entry.model_id.clone()];
    lines.push(limits_line(&model.entry));
    if let Some(line) = endpoint_line(model) {
        lines.push(line);
    }
    if let Some(reason) = &model.unavailable_reason {
        lines.push(reason_line(reason));
    }
    lines
}

fn limits_line(entry: &Entry) -> String {
    let limit = |value: Option<u64>| match value {
        Some(value) => count(value),
        None => t("undeclared").to_owned(),
    };
    tf(
        "Context window {window} · max output {output}",
        &[
            ("window", &limit(entry.capabilities.context_window)),
            ("output", &limit(entry.capabilities.max_output_tokens)),
        ],
    )
}

fn endpoint_line(model: &Model) -> Option<String> {
    let endpoint = model.endpoint.as_deref()?;
    Some(match &model.endpoint_override {
        Some(override_value) => tf(
            "Endpoint {endpoint} · override {value}",
            &[("endpoint", endpoint), ("value", override_value)],
        ),
        None => tf("Endpoint {endpoint}", &[("endpoint", endpoint)]),
    })
}

/// How identification read this entry's id, in a word.
fn identification_line(identification: &crate::wire::Identification) -> String {
    if identification.rule == "ambiguous" {
        let candidates: Vec<String> = identification
            .candidates
            .iter()
            .map(|candidate| format!("{}/{}", candidate.provider_id, candidate.model_id))
            .collect();
        return tf(
            "Identified {rule}",
            &[(
                "rule",
                &format!(
                    "{}, the rows that name it disagree: {}",
                    t("ambiguous"),
                    candidates.join(", ")
                ),
            )],
        );
    }
    let rule = match identification.rule.as_str() {
        "provider-exact" => t("from the Provider's own list"),
        "qualified" => t("from the whole qualified id"),
        "token" => t("by its tokens"),
        "canonical" => t("by its canonical id or display name"),
        "custom-provider" => t("from the custom-provider list"),
        "merge" => t("by merging the rows that name it"),
        other => other,
    };
    tf("Identified {rule}", &[("rule", rule)])
}

/// A model the owner reports unusable says why, in the owner's vocabulary.
fn reason_line(reason: &str) -> String {
    match reason {
        "provider_unavailable" => t("The provider reports it unavailable.").to_owned(),
        "limits_undeclared" => t("Limits undeclared; edit the model to declare them.").to_owned(),
        "account_required" => t("Sign in to the account to use its models.").to_owned(),
        "account_changed" => {
            t("The signed-in account changed; reload to list its models.").to_owned()
        }
        other => other.to_owned(),
    }
}

/// The whole descriptor of one model, as an inspect reports it.
fn descriptor(model: &Model) -> ManagementDetail {
    let entry = &model.entry;
    let mut lines = vec![
        tf("ID {id}", &[("id", &entry.model_id)]),
        tf(
            "Source {source}",
            &[("source", &source_word(&model.source))],
        ),
        tf("Status {status}", &[("status", &entry.status)]),
        limits_line(entry),
    ];
    if let Some(line) = endpoint_line(model) {
        lines.push(line);
    }
    let capability = |value: bool| {
        if value {
            t("yes").to_owned()
        } else {
            t("no").to_owned()
        }
    };
    lines.push(tf(
        "Tools {value}",
        &[("value", &capability(entry.capabilities.tool_calling))],
    ));
    lines.push(tf(
        "Vision {value}",
        &[("value", &capability(entry.capabilities.vision))],
    ));
    let efforts = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.reasoning.as_ref())
        .and_then(|reasoning| reasoning.effort_options.as_deref())
        .unwrap_or_default();
    let reasoning = if !entry.capabilities.reasoning {
        t("not supported").to_owned()
    } else if efforts.is_empty() {
        t("supported").to_owned()
    } else {
        format!("{} ({})", t("supported"), efforts.join(", "))
    };
    lines.push(tf("Reasoning {value}", &[("value", &reasoning)]));
    lines.push(tf(
        "Modalities {list}",
        &[("list", &entry.capabilities.modalities.join(", "))],
    ));
    if let Some(metadata) = &entry.metadata {
        if let Some(description) = &metadata.description {
            lines.push(tf("Description {text}", &[("text", description)]));
        }
        if let Some(date) = &metadata.release_date {
            lines.push(tf("Release date {date}", &[("date", date)]));
        }
        if let Some(pricing) = &metadata.pricing {
            let mut parts = Vec::new();
            if let Some(value) = pricing.input_token_price {
                parts.push(format!("{value} in"));
            }
            if let Some(value) = pricing.output_token_price {
                parts.push(format!("{value} out"));
            }
            if let Some(currency) = &pricing.currency {
                parts.push(currency.clone());
            }
            lines.push(tf("Pricing {value}", &[("value", &parts.join(" "))]));
        }
        if let Some(limit) = &metadata.rate_limit {
            let mut parts = Vec::new();
            if let Some(value) = limit.requests_per_minute {
                parts.push(format!("{value}/min requests"));
            }
            if let Some(value) = limit.tokens_per_minute {
                parts.push(format!("{value}/min tokens"));
            }
            lines.push(tf("Rate limit {value}", &[("value", &parts.join(" · "))]));
        }
        if let Some(id) = &metadata.matched_provider_id {
            lines.push(tf("Matched provider {id}", &[("id", id)]));
        }
        if let Some(id) = &metadata.matched_model_id {
            lines.push(tf("Matched model {id}", &[("id", id)]));
        }
        if let Some(strategy) = &metadata.match_strategy {
            lines.push(tf("Match strategy {strategy}", &[("strategy", strategy)]));
        }
        if !metadata.capability_override_fields.is_empty() {
            lines.push(tf(
                "Capability overrides {fields}",
                &[("fields", &metadata.capability_override_fields.join(", "))],
            ));
        }
        if let Some(identification) = &metadata.identification {
            lines.push(identification_line(identification));
            for limit in &identification.filled {
                lines.push(tf(
                    "{field} {value} supplied by {provider}/{model}",
                    &[
                        ("field", &limit.field),
                        ("value", &count(limit.value)),
                        ("provider", &limit.provider_id),
                        ("model", &limit.model_id),
                    ],
                ));
            }
        }
    }
    if let Some(reason) = &model.unavailable_reason {
        lines.push(reason_line(reason));
    }
    ManagementDetail {
        title: entry.display_name.clone(),
        lines,
    }
}

/// The heading word an entry's source group uses.
fn source_word(source: &str) -> String {
    match source {
        "default" => t("catalog").to_owned(),
        "override" => t("overrides").to_owned(),
        "user" => t("added").to_owned(),
        other => other.to_owned(),
    }
}

/// The document an edit starts from: the entry's effective values, in the
/// shape `provider.models.edit` takes.
pub fn edit_template(provider: &Provider, model: &Model) -> Value {
    let entry = &model.entry;
    let mut document = Map::new();
    document.insert("displayName".into(), json!(entry.display_name));
    if let Some(window) = entry.capabilities.context_window {
        document.insert("contextWindow".into(), json!(window));
    }
    if let Some(tokens) = entry.capabilities.max_output_tokens {
        document.insert("maxOutputTokens".into(), json!(tokens));
    }
    document.insert("toolCalling".into(), json!(entry.capabilities.tool_calling));
    document.insert("vision".into(), json!(entry.capabilities.vision));
    let supported = entry.capabilities.reasoning;
    let mut reasoning = Map::new();
    reasoning.insert("supported".into(), json!(supported));
    if supported
        && let Some(options) = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.reasoning.as_ref())
            .and_then(|reasoning| reasoning.effort_options.as_ref())
        && !options.is_empty()
    {
        reasoning.insert("effortOptions".into(), json!(options));
    }
    document.insert("reasoning".into(), Value::Object(reasoning));
    if let Some(capabilities) = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.raw.get("modalityCapabilities"))
    {
        document.insert("modalityCapabilities".into(), capabilities.clone());
    }
    if provider
        .connection
        .as_ref()
        .is_some_and(|connection| connection.driver_kind == "new-api-router")
        && let Some(endpoint) = &model.endpoint_override
    {
        document.insert(
            "endpoint".into(),
            json!({"action": "set", "value": endpoint}),
        );
    }
    if let Some(alias) = &model.alias {
        document.insert("alias".into(), json!({"action": "set", "value": alias}));
    }
    Value::Object(document)
}

/// The document a new model starts from. Both limits are left undeclared:
/// the editor is where they are declared.
fn add_template() -> Value {
    json!({
        "modelId": "example-model",
        "displayName": "Example model",
        "status": "active",
        "capabilities": {
            "modalities": ["text"],
            "toolCalling": true,
            "streaming": true,
            "structuredOutput": true,
            "vision": false,
            "audio": false,
            "reasoning": false
        }
    })
}
