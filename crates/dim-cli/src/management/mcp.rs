//! The MCP area: the effective catalog and the configuration behind each
//! entry (`mcp.catalog`, `mcp.configuration`, `mcp.update`, `mcp.save`,
//! `mcp.remove`, `mcp.set_enabled`, `mcp.test`, `mcp.inspect`).
//!
//! An entry is one server, named by its id. The catalog reports the effective
//! configuration, one entry per id, so the adapter groups its servers by
//! source and reads each entry's configuration once: that read supplies both
//! the detail lines and the edit document. The edit document is the
//! `mcp.update` patch — whose omitted fields keep their stored values — never
//! the projection, which cannot be written back. A plugin entry has no
//! Settings source, so it has no document and offers no write.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Groups;
use crate::client::Client;
use crate::mcp::{Auth, Catalog, Resolved, Server, resolve};
use crate::model::session::{
    ActionKind, EntryDocument, EntryState, ManagementAction, ManagementDetail, ManagementEntry,
    ManagementView, RemoveVerb,
};
use crate::sanitize::safe_line;
use crate::ui::i18n::{t, tf};

/// The owner's sentence for a write against a plugin entry: the client
/// refuses one before it sends anything, and relays the same words an owner
/// would.
const PLUGIN: &str = "plugin MCP entries change through their plugin";

/// One entry's `mcp.configuration` reply: the revision a write takes (absent
/// for a plugin entry) and the projection a client may show.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Editable {
    #[serde(default)]
    pub revision: Option<String>,
    pub configuration: Value,
}

/// The projection, typed: what a client may show of an entry, with every
/// secret withheld.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Configuration {
    Stdio {
        command: String,
        #[serde(rename = "argsCount")]
        args_count: u64,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(rename = "envNames")]
        env_names: Vec<String>,
        /// The entry's `tools/call` limit in milliseconds; absent means the
        /// default.
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(rename = "referencedVariables", default)]
        referenced_variables: Vec<String>,
        #[serde(rename = "unsetVariables", default)]
        unset_variables: Vec<String>,
    },
    Http(Remote),
    Sse(Remote),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    endpoint: String,
    header_names: Vec<String>,
    auth_configured: bool,
    oauth_configured: bool,
    #[serde(default)]
    oauth: Option<OAuthView>,
    /// The entry's `tools/call` limit in milliseconds; absent means the
    /// default.
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    referenced_variables: Vec<String>,
    #[serde(default)]
    unset_variables: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthView {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    callback_port: Option<u16>,
    #[serde(default)]
    scopes: Option<String>,
    #[serde(default)]
    metadata_configured: bool,
}

impl Configuration {
    fn decode(configuration: &Value) -> Result<Self, String> {
        serde_json::from_value(configuration.clone())
            .map_err(|error| format!("mcp.configuration: {error}"))
    }

    fn transport(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http(_) => "http",
            Self::Sse(_) => "sse",
        }
    }

    /// The configuration lines below the list, as the projection reports
    /// them: no stored value a secret hides, and no value inferred.
    fn lines(&self) -> Vec<String> {
        let mut lines = match self {
            Self::Stdio {
                command,
                args_count,
                cwd,
                env_names,
                ..
            } => vec![
                tf("Executable: {command}", &[("command", &safe_line(command))]),
                tf(
                    "Stored arguments (hidden): {count}",
                    &[("count", &args_count.to_string())],
                ),
                tf(
                    "Working directory: {cwd}",
                    &[("cwd", &cwd.as_deref().map_or("—".to_owned(), safe_line))],
                ),
                tf(
                    "Environment names: {names}",
                    &[("names", &names(env_names))],
                ),
            ],
            Self::Http(remote) | Self::Sse(remote) => {
                let oauth = match &remote.oauth {
                    Some(oauth) if oauth.client_id.is_some() => tf(
                        "OAuth: configured (client ID {id})",
                        &[(
                            "id",
                            &safe_line(oauth.client_id.as_deref().unwrap_or_default()),
                        )],
                    ),
                    Some(_) => t("OAuth: configured").to_owned(),
                    None if remote.oauth_configured => t("OAuth: configured").to_owned(),
                    None => t("OAuth: not configured").to_owned(),
                };
                let metadata = remote
                    .oauth
                    .as_ref()
                    .is_some_and(|oauth| oauth.metadata_configured);
                vec![
                    tf(
                        "Endpoint: {endpoint}",
                        &[("endpoint", &safe_line(&remote.endpoint))],
                    ),
                    tf(
                        "Header names: {names}",
                        &[("names", &names(&remote.header_names))],
                    ),
                    tf(
                        "Bearer token: {state}",
                        &[(
                            "state",
                            t(if remote.auth_configured {
                                "configured"
                            } else {
                                "none"
                            }),
                        )],
                    ),
                    oauth,
                    tf(
                        "Metadata URL: {state}",
                        &[("state", t(if metadata { "configured" } else { "none" }))],
                    ),
                ]
            }
        };
        let (referenced, unset, timeout) = match self {
            Self::Stdio {
                referenced_variables,
                unset_variables,
                timeout,
                ..
            } => (referenced_variables, unset_variables, timeout),
            Self::Http(remote) | Self::Sse(remote) => (
                &remote.referenced_variables,
                &remote.unset_variables,
                &remote.timeout,
            ),
        };
        if let Some(timeout) = timeout {
            lines.push(tf(
                "Tool call timeout: {millis} ms",
                &[("millis", &timeout.to_string())],
            ));
        }
        lines.push(tf("Variables: {names}", &[("names", &names(referenced))]));
        lines.push(tf("Unset variables: {names}", &[("names", &names(unset))]));
        lines
    }
}

/// The `mcp.update` patch, mirroring the owner's `ConfigurationPatch`
/// (`crates/dim-app-server/src/mcp/management.rs`): a field left out keeps
/// its stored value, a present value replaces it, and a map changes only the
/// keys it names. The owner's real-daemon test in
/// `crates/dimcode/tests/cli_flow/mcp.rs` keeps the two in step.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Patch {
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    command: Option<String>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    args: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    cwd: Option<String>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    url: Option<String>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    env: Option<BTreeMap<String, Option<String>>>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    headers: Option<BTreeMap<String, Option<String>>>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    bearer: Option<BearerPatch>,
    /// A present `null` clears the entry's limit.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    timeout: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    oauth: Option<OAuthPatch>,
}

/// A field the owner takes as a value, never as `null`: present `null` is a
/// refusal, as it is for the owner's decoder.
fn non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A field the owner takes as nullable: present `null` clears it.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum BearerPatch {
    Set { token: String },
    Remove,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum OAuthPatch {
    Set {
        #[serde(
            rename = "clientId",
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "Option::is_none"
        )]
        client_id: Option<Option<String>>,
        #[serde(
            rename = "callbackPort",
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "Option::is_none"
        )]
        callback_port: Option<Option<u16>>,
        #[serde(
            rename = "authServerMetadataUrl",
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "Option::is_none"
        )]
        auth_server_metadata_url: Option<Option<String>>,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "Option::is_none"
        )]
        scopes: Option<Option<String>>,
    },
    Remove,
}

/// The add document: the id, the source it saves to, and the definition
/// `mcp.save` takes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewEntry {
    pub id: String,
    pub source: String,
    pub configuration: Value,
}

/// One entry's enablement, as `mcp enable|disable` and the panel's toggle
/// report it: the source the write went to, the sentence naming the entry,
/// and what the owner's rules add.
pub struct Enabled {
    pub source: String,
    pub id: String,
    pub enabled: bool,
    pub notes: Vec<String>,
}

impl Enabled {
    /// The line the standalone command and both `/mcp` clients print.
    pub fn sentence(&self) -> String {
        tf(
            if self.enabled {
                "Enabled {id} ({source})."
            } else {
                "Disabled {id} ({source})."
            },
            &[
                ("id", &format!("{:?}", self.id)),
                ("source", &safe_line(&self.source)),
            ],
        )
    }

    /// What the panel shows as its message: the sentence, then the notes.
    pub fn note(&self) -> String {
        let mut text = self.sentence();
        for note in &self.notes {
            text.push(' ');
            text.push_str(note);
        }
        text
    }
}

/// Enables or disables one entry, as `mcp enable|disable`, both `/mcp`
/// clients and the panel's toggle do: one owner operation, one set of rules.
///
/// `source` is the source the command named or the row reported; without one
/// the entry is the only one the catalog lists for `id`, and an id more than
/// one source lists is refused rather than guessed. A disable withdraws the
/// workspace's enablement when the catalog cannot be read, and a project
/// enable says what it turned on. A plugin entry is refused before the
/// write, since its source has no Settings document.
pub async fn set_enabled(
    client: &mut Client,
    cwd: &str,
    id: &str,
    source: Option<&str>,
    enabled: bool,
) -> crate::Result<Enabled> {
    // Only the workspace's own record can be withdrawn without a readable
    // source, so a named source that is not the project's cannot take this
    // path.
    let withdraws_workspace = !enabled && source.is_none_or(|source| source == "project");
    let catalog: Catalog = match client.typed("mcp.catalog", json!({"cwd": cwd})).await {
        Ok(catalog) => catalog,
        Err(error)
            if withdraws_workspace && error.downcast_ref::<crate::client::RpcError>().is_some() =>
        {
            withdraw(client, cwd, id, String::new()).await?;
            return Ok(Enabled {
                source: "project".into(),
                id: id.into(),
                enabled: false,
                notes: vec![tf(
                    "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged.",
                    &[
                        ("error", &safe_line(&error.to_string())),
                        ("id", &format!("{id:?}")),
                    ],
                )],
            });
        }
        Err(error) => return Err(error),
    };
    let entry = match resolve(&catalog, id, source) {
        Ok(Some(entry)) => entry,
        Err(sources) => return Err(more_than_one(id, &sources).into()),
        // A disable of an id no source lists withdraws this workspace's
        // record and nothing else.
        Ok(None) if withdraws_workspace => {
            let Some(expected) = catalog.project_revision.clone() else {
                return Err(format!("No MCP server {id:?} in mcp list").into());
            };
            withdraw(client, cwd, id, expected).await?;
            return Ok(Enabled {
                source: "project".into(),
                id: id.to_owned(),
                enabled: false,
                notes: vec![tf(
                    "{id} is not in mcp list; withdrew any stored enablement of it in this workspace.",
                    &[("id", &format!("{id:?}"))],
                )],
            });
        }
        Ok(None) => {
            return Err(match source {
                Some(source) => unlisted(id, Some(source)),
                None => format!("No MCP server {id:?} in mcp list"),
            }
            .into());
        }
    };
    let source = match entry {
        Resolved::Server(server) => {
            if server.source == "plugin" {
                return Err(PLUGIN.into());
            }
            server.source
        }
        // A broken entry is not enableable; its own error says why.
        Resolved::Broken(entry) => return Err(entry.error.into()),
    };
    let expected = if source == "project" {
        catalog
            .project_revision
            .ok_or("Invalid mcp.catalog response")?
    } else {
        catalog.user_revision
    };
    client
        .rpc(
            "mcp.set_enabled",
            json!({"cwd":cwd,"id":id,"source":source,"expectedVersion":expected,"enabled":enabled}),
        )
        .await?;
    let mut notes = Vec::new();
    if enabled && source == "project" {
        notes.push(t("It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions.").to_owned());
    }
    Ok(Enabled {
        source,
        id: id.to_owned(),
        enabled,
        notes,
    })
}

/// Withdraws this workspace's enablement of `id` at `expected`: the one
/// write a disable makes without the entry, since only the project record
/// needs none.
async fn withdraw(client: &mut Client, cwd: &str, id: &str, expected: String) -> crate::Result<()> {
    client
        .rpc(
            "mcp.set_enabled",
            json!({"cwd":cwd,"id":id,"source":"project","expectedVersion":expected,"enabled":false}),
        )
        .await?;
    Ok(())
}

/// Why an MCP action did not happen.
pub enum Failure {
    /// The stored settings changed since they were read: nothing was
    /// written, and this is the area as it now is, or why the read that
    /// reports it failed.
    Conflict {
        message: String,
        catalog: Result<Box<Catalog>, String>,
    },
    /// The action was applied and the read that reports the area failed.
    ReadFailed { note: Option<String>, error: String },
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
            Self::ReadFailed { error: message, .. } | Self::Refused(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// What an MCP action came to.
pub enum Answer {
    /// The area after the action, and what the owner's reply adds beyond it.
    Area {
        catalog: Box<Catalog>,
        note: Option<String>,
    },
    /// A test's tools, typed, so each surface formats them itself.
    Tools { id: String, tools: Vec<Tool> },
    /// An inspect: a detail, not a view.
    Detail(ManagementDetail),
}

/// What an import does with one entry of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportAction {
    Import,
    Skip,
    Replace,
    Rename,
}

impl ImportAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Skip => "skip",
            Self::Replace => "replace",
            Self::Rename => "rename",
        }
    }
}

/// One entry's resolution in `mcp.import.apply`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportResolution {
    pub id: String,
    pub action: ImportAction,
    /// The id a rename writes under; absent for every other action.
    pub new_id: Option<String>,
}

impl ImportResolution {
    fn value(&self) -> Value {
        let mut value = json!({"id": self.id, "action": self.action.as_str()});
        if let Some(new_id) = &self.new_id {
            value["newId"] = json!(new_id);
        }
        value
    }
}

/// One known store `mcp.import.sources` reports. It carries the store's
/// identity, the file read, its state and how many entries it holds or the
/// owner's message; never an entry id or a value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    pub id: String,
    pub client: String,
    pub scope: String,
    pub path: String,
    pub state: ImportSourceState,
    #[serde(default)]
    pub entries: Option<usize>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ImportSource {
    /// How many entries the store holds, or the owner's message for a store
    /// that is not ready.
    pub fn detail(&self) -> String {
        match (&self.entries, &self.error) {
            (Some(entries), _) => entries.to_string(),
            (_, error) => error.clone().unwrap_or_else(|| "—".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportSourceState {
    Ready,
    Absent,
    Unreadable,
    TooLarge,
    Malformed,
}

impl ImportSourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Absent => "absent",
            Self::Unreadable => "unreadable",
            Self::TooLarge => "too-large",
            Self::Malformed => "malformed",
        }
    }
}

#[derive(Debug, Deserialize)]
struct ImportSources {
    sources: Vec<ImportSource>,
}

/// The preview `mcp.import.preview` reports: what importing the document
/// would do to one source, read at `revision`. It names fields and kinds,
/// never a value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub revision: String,
    pub entries: Vec<ImportEntryPreview>,
    /// Top-level members other than `mcpServers`; an import does not carry
    /// them.
    #[serde(default)]
    pub unknown_top_level: Vec<String>,
    /// With `from`: the digest of the store document the preview read; absent
    /// for a client-supplied `document`, which the apply carries itself.
    #[serde(default)]
    pub source_revision: Option<String>,
    /// With `from`: the known store the preview read, named as
    /// `mcp.import.sources` names it; absent for a client-supplied document.
    #[serde(default)]
    pub store: Option<ImportSourceRef>,
}

/// One known store's identity and display path, without its entries or state.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceRef {
    pub id: String,
    pub client: String,
    pub scope: String,
    pub path: String,
}

/// One entry's place in the preview.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEntryPreview {
    pub id: String,
    #[serde(default)]
    pub transport: Option<String>,
    pub status: ImportStatus,
    /// The fields a write would change, with the kind each holds.
    #[serde(default)]
    pub changes: Vec<ImportField>,
    /// Members the decoder keeps unused or drops.
    #[serde(default)]
    pub unsupported: Vec<ImportUnsupported>,
    /// How many field names the owner's cap left out of `changes` and
    /// `unsupported`.
    #[serde(default)]
    pub omitted: usize,
    #[serde(default)]
    pub error: Option<String>,
}

/// What an import would do with one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportStatus {
    /// The source already holds this entry and equal content.
    Identical,
    /// The source would gain this entry.
    Changed,
    /// The source holds this id with different content.
    Conflict,
    /// The entry does not decode.
    Invalid,
}

impl ImportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Changed => "changed",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
        }
    }

    /// Whether an entry with this status is accepted without a flag. A
    /// conflict or an invalid entry is skipped until a flag names it.
    pub fn imports(self) -> bool {
        matches!(self, Self::Identical | Self::Changed)
    }
}

/// One field a write would change, with the kind of value it holds.
#[derive(Debug, Deserialize)]
pub struct ImportField {
    pub field: String,
    pub kind: String,
}

/// One entry member the decoder does not act on.
#[derive(Debug, Deserialize)]
pub struct ImportUnsupported {
    pub field: String,
    pub effect: UnsupportedEffect,
}

/// What the decoder does with a member it does not act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnsupportedEffect {
    /// Kept in the entry's extension members and not acted on.
    KeptUnused,
    /// Dropped by the decoder.
    Dropped,
}

impl UnsupportedEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeptUnused => "kept-unused",
            Self::Dropped => "dropped",
        }
    }
}

/// The apply's reply: the source revision the import wrote.
#[derive(Debug, Deserialize)]
pub struct ImportApplied {
    pub revision: String,
}

/// The known stores of other local clients, read at most once per call. The
/// owner reads the files; the reply carries no entry and no value.
pub async fn import_sources(client: &mut Client, cwd: &str) -> crate::Result<Vec<ImportSource>> {
    let reply: ImportSources = client
        .typed("mcp.import.sources", json!({"cwd": cwd}))
        .await?;
    Ok(reply.sources)
}

/// Where an import's document comes from: the client's own text, or one known
/// store of another local client that the owner reads.
#[derive(Clone, Copy)]
pub enum ImportInput<'a> {
    Document(&'a str),
    Store(&'a str),
}

impl ImportInput<'_> {
    fn write(self, params: &mut Value) {
        match self {
            Self::Document(document) => params["document"] = json!(document),
            Self::Store(id) => params["from"] = json!(id),
        }
    }
}

/// The entries the request names, previewed against the source `source`
/// names. It writes and executes nothing.
pub async fn preview_import(
    client: &mut Client,
    cwd: &str,
    source: &str,
    input: ImportInput<'_>,
) -> crate::Result<ImportPreview> {
    let mut params = json!({"cwd": cwd, "source": source});
    input.write(&mut params);
    client.typed("mcp.import.preview", params).await
}

/// Writes the entries `resolutions` accept into `source`, all or nothing, at
/// the revision the preview read (`expected`). With `from`, it also carries
/// the store digest the preview answered, so the owner applies the text it
/// previewed and nothing else. It answers the new revision.
pub async fn apply_import(
    client: &mut Client,
    cwd: &str,
    source: &str,
    input: ImportInput<'_>,
    expected: &str,
    expected_source_revision: Option<String>,
    resolutions: &[ImportResolution],
) -> crate::Result<String> {
    let mut params = json!({
        "cwd": cwd,
        "source": source,
        "expectedVersion": expected,
        "resolutions": resolutions.iter().map(ImportResolution::value).collect::<Vec<_>>(),
    });
    input.write(&mut params);
    if let Some(revision) = expected_source_revision {
        params["expectedSourceRevision"] = json!(revision);
    }
    let applied: ImportApplied = client.typed("mcp.import.apply", params).await?;
    Ok(applied.revision)
}

/// Why an import did not happen, in the CLI's words. A refused entry or
/// document keeps the owner's sentence, which names the entry and the next
/// step; the other codes say what the caller must do differently.
pub fn import_failure(error: crate::Error) -> crate::Error {
    match crate::client::app_error(&error) {
        Some("mcp_settings_conflict") => t(
            "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again.",
        )
        .into(),
        Some("mcp_import_partial") => t(
            "The project entries were written and their withheld values were not; replace them again to retry.",
        )
        .into(),
        _ => error,
    }
}

/// The catalog as its owner reports it.
pub async fn catalog(client: &mut Client, cwd: &str) -> Result<Catalog, String> {
    client
        .typed("mcp.catalog", json!({"cwd": cwd}))
        .await
        .map_err(|error| error.to_string())
}

/// The effective entry `(id, source)` names, or why the area has none.
///
/// `source` is the source the command named or the row reported; without one
/// the entry is the only one the catalog lists for `id`. A broken entry
/// answers with its own error: it has no effective configuration to test or
/// inspect.
pub async fn target(
    client: &mut Client,
    cwd: &str,
    id: &str,
    source: Option<&str>,
) -> Result<Server, String> {
    let catalog = catalog(client, cwd).await?;
    match resolve(&catalog, id, source) {
        Ok(Some(entry)) => entry.server(),
        Err(sources) => Err(more_than_one(id, &sources)),
        Ok(None) => Err(unlisted(id, source)),
    }
}

/// The params `mcp.auth.*` and `mcp.disconnect` take for the entry `id`
/// names: the source a command named or the only one the catalog lists.
pub async fn target_params(
    client: &mut Client,
    cwd: &str,
    id: &str,
    source: Option<&str>,
) -> crate::Result<Value> {
    let server = target(client, cwd, id, source).await?;
    Ok(json!({"id": server.id, "source": server.source, "cwd": cwd}))
}

/// One entry's configuration as its owner reports it.
pub async fn editable(client: &mut Client, cwd: &str, server: &Server) -> Result<Editable, String> {
    client
        .typed(
            "mcp.configuration",
            json!({"cwd": cwd, "id": server.id, "source": server.source}),
        )
        .await
        .map_err(|error| error.to_string())
}

/// The edit document, seeded from one configuration read: what the
/// projection may show, with an empty map for the secrets it withholds.
/// Stored names are not seeded, since a patch's `null` removes and a
/// placeholder would be written literally.
pub fn seed(configuration: &Value) -> Result<Value, String> {
    Ok(match Configuration::decode(configuration)? {
        Configuration::Stdio {
            command,
            cwd,
            timeout,
            ..
        } => {
            let mut document = json!({
                "command": command,
                "cwd": cwd.unwrap_or_default(),
                "env": {},
            });
            if let Some(timeout) = timeout {
                document["timeout"] = json!(timeout);
            }
            document
        }
        Configuration::Http(remote) | Configuration::Sse(remote) => {
            let mut document = json!({"headers": {}});
            if let Some(timeout) = remote.timeout {
                document["timeout"] = json!(timeout);
            }
            if let Some(oauth) = remote.oauth {
                let mut set = serde_json::Map::new();
                set.insert("action".into(), json!("set"));
                if let Some(client_id) = oauth.client_id {
                    set.insert("clientId".into(), json!(client_id));
                }
                if let Some(port) = oauth.callback_port {
                    set.insert("callbackPort".into(), json!(port));
                }
                if let Some(scopes) = oauth.scopes {
                    set.insert("scopes".into(), json!(scopes));
                }
                document["oauth"] = Value::Object(set);
            }
            document
        }
    })
}

/// Which conflict a refused write was, since the message names what the
/// client did not do.
#[derive(Clone, Copy)]
enum Conflict {
    Toggle,
    Edit,
    Add,
    Remove,
}

/// The area after a refusal, when the owner refused because the settings
/// changed; any other refusal leaves the draft where it is. A read that
/// fails is still a conflict, and says why it cannot show the area.
async fn refused(client: &mut Client, cwd: &str, error: &crate::Error, kind: Conflict) -> Failure {
    if crate::client::app_error(error) != Some("mcp_settings_conflict") {
        return Failure::Refused(error.to_string());
    }
    let message = match kind {
        Conflict::Toggle => {
            t("This entry's settings changed since they were read; nothing was written.")
        }
        Conflict::Edit => t("The MCP entry changed since it was read; nothing was written."),
        Conflict::Add => t("The MCP entries changed since they were read; nothing was written."),
        Conflict::Remove => t("The MCP entry changed since it was read; nothing was removed."),
    };
    Failure::Conflict {
        message: message.to_owned(),
        catalog: catalog(client, cwd).await.map(Box::new),
    }
}

/// The refusal for an id more than one source lists: it names every source
/// and the flag that settles one.
fn more_than_one(id: &str, sources: &[String]) -> String {
    tf(
        "{id} is in more than one MCP source: {sources}. Name one with --source.",
        &[("id", &format!("{id:?}")), ("sources", &sources.join(", "))],
    )
}

/// The refusal for an id no source lists, or that the source a reader named
/// does not.
fn unlisted(id: &str, source: Option<&str>) -> String {
    match source {
        Some(source) => tf(
            "No MCP server {id} is in source {source}.",
            &[
                ("id", &format!("{id:?}")),
                ("source", &format!("{source:?}")),
            ],
        ),
        None => t("No MCP server has that id.").to_owned(),
    }
}

/// The refusal for an action on one entry that names no source: a row always
/// carries the source its owner reported, and the adapter never searches the
/// catalog for one.
const NO_SOURCE: &str =
    "The action names no MCP source; a row carries the source its owner reported.";

/// Performs `action` for the row it came from, then reads the area back as
/// its owner reports it.
///
/// A toggle and a remove write at the source revision the catalog reports
/// immediately before them, so the compare-and-swap covers that window only;
/// both are targeted at one id, so a change elsewhere in the source is not
/// overwritten. An add checks the id against the same read that supplies its
/// revision, so a concurrent add of that id fails the swap instead of being
/// replaced. The write's own answer is kept when the read after it fails; a
/// read that fails is the answer itself, since nothing was applied.
pub async fn apply(
    client: &mut Client,
    action: &ManagementAction,
    cwd: &str,
) -> Result<Answer, Failure> {
    perform(client, action, cwd, row_source(action)?).await
}

/// Performs `action` for a command line that named `source`, or none: the
/// resolver then settles the id, and one more than a source lists is refused
/// rather than guessed.
pub async fn apply_named(
    client: &mut Client,
    action: &ManagementAction,
    cwd: &str,
    source: Option<&str>,
) -> Result<Answer, Failure> {
    perform(client, action, cwd, source).await
}

/// The source of the row `action` came from. A row always carries one, so an
/// action without it is refused instead of searched for.
fn row_source(action: &ManagementAction) -> Result<Option<&str>, Failure> {
    let source = match action {
        ManagementAction::Toggle { source, .. }
        | ManagementAction::Save { source, .. }
        | ManagementAction::Remove { source, .. }
        | ManagementAction::Run { source, .. }
        | ManagementAction::Inspect { source, .. } => source.as_deref(),
        ManagementAction::List | ManagementAction::Reload | ManagementAction::Add { .. } => {
            return Ok(None);
        }
    };
    source
        .map(Some)
        .ok_or_else(|| Failure::Refused(t(NO_SOURCE).to_owned()))
}

/// `apply` for one row or one command line, with the source already decided:
/// `None` leaves the resolver to settle the id.
async fn perform(
    client: &mut Client,
    action: &ManagementAction,
    cwd: &str,
    source: Option<&str>,
) -> Result<Answer, Failure> {
    let mut note = None;
    match action {
        ManagementAction::List | ManagementAction::Reload => {}
        ManagementAction::Toggle { id, enabled, .. } => {
            match set_enabled(client, cwd, id, source, *enabled).await {
                Ok(state) => note = Some(state.note()),
                Err(error) => return Err(refused(client, cwd, &error, Conflict::Toggle).await),
            }
        }
        ManagementAction::Save {
            id,
            expected_revision,
            document,
            ..
        } => {
            let Some(id) = id else {
                return Err(Failure::Refused(
                    t("MCP's documents belong to one entry; this save names none.").to_owned(),
                ));
            };
            let patch: Patch = serde_json::from_value(document.clone())
                .map_err(|error| Failure::Refused(format!("mcp.update: {error}")))?;
            let catalog = catalog(client, cwd).await.map_err(Failure::Refused)?;
            let server = resolved(&catalog, id, source)?
                .server()
                .map_err(Failure::Refused)?;
            if server.source == "plugin" {
                return Err(Failure::Refused(PLUGIN.to_owned()));
            }
            let patch = serde_json::to_value(&patch)
                .map_err(|error| Failure::Refused(error.to_string()))?;
            let written = client
                .rpc(
                    "mcp.update",
                    json!({
                        "cwd": cwd,
                        "id": id,
                        "source": server.source,
                        "expectedVersion": expected_revision,
                        "patch": patch,
                    }),
                )
                .await;
            if let Err(error) = written {
                return Err(refused(client, cwd, &error, Conflict::Edit).await);
            }
        }
        ManagementAction::Add { document } => {
            let entry: NewEntry = serde_json::from_value(document.clone())
                .map_err(|error| Failure::Refused(format!("mcp.save: {error}")))?;
            if entry.id.trim().is_empty() {
                return Err(Failure::Refused(
                    t("The add document needs an id.").to_owned(),
                ));
            }
            if !matches!(entry.source.as_str(), "user" | "project") {
                return Err(Failure::Refused(
                    t("The add document needs a source of user or project.").to_owned(),
                ));
            }
            if !entry.configuration.is_object() {
                return Err(Failure::Refused(
                    t("The add document needs a configuration object.").to_owned(),
                ));
            }
            let catalog = catalog(client, cwd).await.map_err(Failure::Refused)?;
            if let Some(existing) = catalog.servers.iter().find(|server| server.id == entry.id) {
                // A plugin entry is removed through its plugin, so the only
                // step left is another id.
                let message = if existing.source == "plugin" {
                    tf(
                        "{id} is already in mcp list; choose another id, or change it through its plugin.",
                        &[("id", &safe_line(&entry.id))],
                    )
                } else {
                    tf(
                        "{id} is already in mcp list; remove it first or choose another id.",
                        &[("id", &safe_line(&entry.id))],
                    )
                };
                return Err(Failure::Refused(message));
            }
            let expected = if entry.source == "project" {
                catalog.project_revision.ok_or_else(|| {
                    Failure::Refused(t("This workspace has no project MCP source.").to_owned())
                })?
            } else {
                catalog.user_revision
            };
            // Saving over a broken entry of the same source replaces it.
            let replaced = catalog
                .invalid
                .iter()
                .any(|invalid| invalid.id == entry.id && invalid.source == entry.source);
            let written = client
                .rpc(
                    "mcp.save",
                    json!({
                        "cwd": cwd,
                        "id": entry.id,
                        "source": entry.source,
                        "expectedVersion": expected,
                        "configuration": entry.configuration,
                    }),
                )
                .await;
            if let Err(error) = written {
                return Err(refused(client, cwd, &error, Conflict::Add).await);
            }
            note = Some(if replaced {
                tf("Replaced {id}.", &[("id", &safe_line(&entry.id))])
            } else {
                tf("Added {id}.", &[("id", &safe_line(&entry.id))])
            });
        }
        ManagementAction::Remove { id, .. } => {
            let catalog = catalog(client, cwd).await.map_err(Failure::Refused)?;
            // A broken entry is removable, so only a plugin entry is refused
            // here.
            let source = match resolved(&catalog, id, source)? {
                Resolved::Server(server) => {
                    if server.source == "plugin" {
                        return Err(Failure::Refused(PLUGIN.to_owned()));
                    }
                    server.source
                }
                Resolved::Broken(entry) => entry.source,
            };
            let expected = if source == "project" {
                catalog.project_revision.clone().ok_or_else(|| {
                    Failure::Refused(t("This workspace has no project MCP source.").to_owned())
                })?
            } else {
                catalog.user_revision.clone()
            };
            let written = client
                .rpc(
                    "mcp.remove",
                    json!({
                        "cwd": cwd,
                        "id": id,
                        "source": source,
                        "expectedVersion": expected,
                    }),
                )
                .await;
            if let Err(error) = written {
                return Err(refused(client, cwd, &error, Conflict::Remove).await);
            }
        }
        // A test executes the stored entry and writes nothing the area
        // lists, so it answers with its tools and the area is not re-read.
        ManagementAction::Run { id, document, .. } => {
            // No entry carries a test document and neither client sends one.
            if document.is_some() {
                return Err(Failure::Refused(
                    t("An MCP server's test takes no input document.").to_owned(),
                ));
            }
            return test(client, cwd, id, source).await;
        }
        ManagementAction::Inspect { id, .. } => return inspect(client, cwd, id, source).await,
    }
    match catalog(client, cwd).await {
        Ok(catalog) => Ok(Answer::Area {
            catalog: Box::new(catalog),
            note,
        }),
        // A read applied nothing, so its failure is the answer itself, not a
        // report that follows a write.
        Err(error) if matches!(action, ManagementAction::List | ManagementAction::Reload) => {
            Err(Failure::Refused(error))
        }
        Err(error) => Err(Failure::ReadFailed { note, error }),
    }
}

/// One entry as a `mcp.test` probe reports it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Probe {
    status: String,
    #[serde(default)]
    tools: Option<Vec<Tool>>,
    #[serde(default)]
    expired: Option<bool>,
    #[serde(default)]
    missing_scopes: Option<Vec<String>>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One tool as a detail line: its name, then its description when it has
/// one.
fn tool_line(tool: &Tool) -> String {
    let name = safe_line(&tool.name);
    match tool.description.as_deref().map(safe_line) {
        Some(description) if !description.is_empty() => format!("{name} — {description}"),
        _ => name,
    }
}

/// The panel's detail for a test's tools: one line per tool, or the
/// sentence that says there are none.
pub fn tools_detail(id: &str, tools: &[Tool]) -> ManagementDetail {
    let lines = if tools.is_empty() {
        vec![t("The server exposes no tools.").to_owned()]
    } else {
        tools.iter().map(tool_line).collect()
    };
    ManagementDetail {
        title: tf("Tools of {id}", &[("id", &safe_line(id))]),
        lines,
    }
}

/// One tool as the CLI's report row: `name\tdescription` when it has one,
/// its name alone otherwise.
pub fn tool_row(tool: &Tool) -> String {
    let name = safe_line(&tool.name);
    match tool.description.as_deref().map(safe_line) {
        Some(description) if !description.is_empty() => format!("{name}\t{description}"),
        _ => name,
    }
}

/// Why a probe did not connect, in the owner's words when it has them.
fn probe_failure(probe: &Probe) -> String {
    if let Some(message) = &probe.message {
        return safe_line(message);
    }
    match probe.status.as_str() {
        "authRequired" if probe.expired == Some(true) => {
            t("The server rejected the stored credential; sign in again with mcp login.").to_owned()
        }
        "authRequired" => match &probe.missing_scopes {
            Some(scopes) if !scopes.is_empty() => tf(
                "The server requires additional scopes: {scopes}.",
                &[("scopes", &names(scopes))],
            ),
            _ => t("The server requires authorization; run mcp login ID.").to_owned(),
        },
        status => tf(
            "The probe ended: {status}.",
            &[("status", &safe_line(status))],
        ),
    }
}

/// The one entry `(id, source)` names in `catalog`, or why the area has
/// none. The answer is kept as one entry so a remove can take a broken one
/// while every other action refuses it with its own error.
fn resolved(catalog: &Catalog, id: &str, source: Option<&str>) -> Result<Resolved, Failure> {
    resolve(catalog, id, source)
        .map_err(|sources| Failure::Refused(more_than_one(id, &sources)))?
        .ok_or_else(|| Failure::Refused(unlisted(id, source)))
}

/// `mcp.test`: the tools a temporary connection found, typed for each
/// surface to format.
async fn test(
    client: &mut Client,
    cwd: &str,
    id: &str,
    source: Option<&str>,
) -> Result<Answer, Failure> {
    let server = target(client, cwd, id, source)
        .await
        .map_err(Failure::Refused)?;
    let probe: Probe = client
        .typed(
            "mcp.test",
            json!({"cwd": cwd, "id": server.id, "source": server.source}),
        )
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    if probe.status != "connected" {
        return Err(Failure::Refused(probe_failure(&probe)));
    }
    Ok(Answer::Tools {
        id: server.id,
        tools: probe.tools.unwrap_or_default(),
    })
}

/// One entry's `mcp.inspect` reply: the effective configuration and the
/// Session's live connections. It is a report and is never written back.
#[derive(Debug, Deserialize)]
struct Inspection {
    enabled: bool,
    configuration: Value,
    #[serde(default)]
    connections: Vec<Connection>,
}

#[derive(Debug, Deserialize)]
struct Connection {
    state: ConnectionState,
    failure: Option<String>,
    #[serde(default)]
    tools: Option<Vec<Tool>>,
}

#[derive(Debug, Deserialize)]
struct ConnectionState {
    status: String,
    #[serde(default)]
    expired: Option<bool>,
}

/// `mcp.inspect`: the effective entry and its connections, as a detail.
async fn inspect(
    client: &mut Client,
    cwd: &str,
    id: &str,
    source: Option<&str>,
) -> Result<Answer, Failure> {
    let server = target(client, cwd, id, source)
        .await
        .map_err(Failure::Refused)?;
    let inspection: Inspection = client
        .typed(
            "mcp.inspect",
            json!({"cwd": cwd, "id": server.id, "source": server.source}),
        )
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    let configuration =
        Configuration::decode(&inspection.configuration).map_err(Failure::Refused)?;
    let mut lines = vec![header(
        &server.source,
        configuration.transport(),
        server.auth.as_ref(),
        Some(inspection.enabled),
    )];
    lines.extend(configuration.lines());
    if server.source == "plugin" {
        lines.push(t("Owned by a plugin; change it through Plugins.").to_owned());
    }
    if inspection.connections.is_empty() {
        lines.push(t("No live session connections.").to_owned());
    }
    for (index, connection) in inspection.connections.iter().enumerate() {
        let mut head = tf(
            "Connection {number} · {status}",
            &[
                ("number", &(index + 1).to_string()),
                ("status", &safe_line(&connection.state.status)),
            ],
        );
        if connection.state.expired == Some(true) {
            head.push_str(" expired");
        }
        lines.push(head);
        if let Some(failure) = &connection.failure {
            lines.push(format!("  {}", safe_line(failure)));
        }
        match &connection.tools {
            Some(tools) if !tools.is_empty() => {
                lines.extend(tools.iter().map(|tool| format!("  {}", tool_line(tool))));
            }
            Some(_) => lines.push(format!("  {}", t("The server exposes no tools."))),
            None => lines.push(format!("  {}", t("Tools have not been discovered."))),
        }
    }
    Ok(Answer::Detail(ManagementDetail {
        title: tf("MCP entry {id}", &[("id", &safe_line(&server.id))]),
        lines,
    }))
}

/// The entries as the effective catalog reports them, grouped by source and
/// each with the kinds its source allows.
pub(super) async fn view(
    client: &mut Client,
    cwd: &str,
    catalog: &Catalog,
    available: &[ActionKind],
) -> ManagementView {
    // A daemon older than `mcp.configuration` lists what its catalog carries:
    // no entry detail and no edit, and no entry is invalid for the read it
    // cannot serve.
    let configurations = client.supports(&["mcp.configuration"]);
    let mut groups = Groups::default();
    for source in ["user", "project", "plugin"] {
        for server in catalog
            .servers
            .iter()
            .filter(|server| server.source == source)
        {
            let plugin = server.source == "plugin";
            let mut states = Vec::new();
            let mut lines = Vec::new();
            let mut document = None;
            let edition = if configurations {
                Some(editable(client, cwd, server).await)
            } else {
                None
            };
            match edition {
                // The daemon cannot read a configuration: the entry is listed
                // as the catalog reports it.
                None => lines.push(header(
                    &server.source,
                    &server.transport,
                    server.auth.as_ref(),
                    None,
                )),
                Some(Ok(edition)) => match Configuration::decode(&edition.configuration) {
                    Ok(configuration) => {
                        lines.push(header(
                            &server.source,
                            configuration.transport(),
                            server.auth.as_ref(),
                            None,
                        ));
                        lines.extend(configuration.lines());
                        // A plugin entry has no Settings source, so the
                        // owner reports no revision for it.
                        if let (Some(revision), false) = (&edition.revision, plugin) {
                            match seed(&edition.configuration) {
                                Ok(document_value) => {
                                    document = Some(EntryDocument {
                                        revision: revision.clone(),
                                        document: document_value,
                                    });
                                }
                                Err(error) => {
                                    states.push(EntryState::Invalid);
                                    lines.push(error);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        states.push(EntryState::Invalid);
                        lines.push(header(
                            &server.source,
                            &server.transport,
                            server.auth.as_ref(),
                            None,
                        ));
                        lines.push(error);
                    }
                },
                // A failed configuration read leaves the entry listed with
                // what the catalog says, and no edit.
                Some(Err(error)) => {
                    states.push(EntryState::Invalid);
                    lines.push(header(
                        &server.source,
                        &server.transport,
                        server.auth.as_ref(),
                        None,
                    ));
                    lines.push(error);
                }
            }
            if plugin {
                states.push(EntryState::ReadOnly);
                lines.push(t("Owned by a plugin; change it through Plugins.").to_owned());
            }
            groups.push(
                source,
                ManagementEntry {
                    id: server.id.clone(),
                    source: Some(server.source.clone()),
                    label: server.id.clone(),
                    detail: lines,
                    enabled: Some(server.enabled),
                    states,
                    actions: offers(server, document.is_some(), available),
                    remove_verb: RemoveVerb::Remove,
                    document,
                    run: None,
                },
            );
        }
        // A broken entry has no configuration to toggle, edit, test or
        // inspect. It can be removed, and an add of the same id replaces it.
        for entry in catalog
            .invalid
            .iter()
            .filter(|entry| entry.source == source)
        {
            let mut actions = vec![ActionKind::Remove];
            actions.retain(|kind| available.contains(kind));
            groups.push(
                source,
                ManagementEntry {
                    id: entry.id.clone(),
                    source: Some(entry.source.clone()),
                    label: entry.id.clone(),
                    detail: vec![
                        header(&entry.source, t("invalid"), None, None),
                        safe_line(&entry.error),
                    ],
                    enabled: None,
                    states: vec![EntryState::Invalid],
                    actions,
                    remove_verb: RemoveVerb::Remove,
                    document: None,
                    run: None,
                },
            );
        }
    }
    ManagementView {
        title: None,
        revision: None,
        document: None,
        create: Some(create()),
        actions: super::area_kinds(available),
        groups: groups.into_vec(),
        issues: issues(catalog),
    }
}

/// The kinds one entry offers: no write for a plugin entry, an edit only
/// when its configuration read carried a revision, and a test only for a
/// user entry or an enabled one, as the owner requires.
fn offers(server: &Server, has_document: bool, available: &[ActionKind]) -> Vec<ActionKind> {
    let plugin = server.source == "plugin";
    let mut kinds = Vec::new();
    if !plugin {
        kinds.push(ActionKind::Toggle);
    }
    if has_document {
        kinds.push(ActionKind::Edit);
    }
    if !plugin {
        kinds.push(ActionKind::Remove);
    }
    if server.source == "user" || server.enabled {
        kinds.push(ActionKind::Run);
    }
    kinds.push(ActionKind::Inspect);
    kinds.retain(|kind| available.contains(kind));
    kinds
}

/// One entry's first line: its source, transport and authorization, with
/// the enablement an inspect runs it with when that differs from the list.
fn header(source: &str, transport: &str, auth: Option<&Auth>, enabled: Option<bool>) -> String {
    let mut line = format!("{} · {}", safe_line(source), safe_line(transport));
    if let Some(enabled) = enabled {
        line.push_str(&format!(
            " · {}",
            t(if enabled { "enabled" } else { "disabled" })
        ));
    }
    if let Some(auth) = auth {
        let status = match auth.status.as_str() {
            "required" => t("sign-in required"),
            "authorized" => t("signed in"),
            "expired" => t("sign-in expired"),
            other => other,
        };
        line.push_str(&format!(" · {}", safe_line(status)));
        if let Some(scopes) = &auth.missing_scopes
            && !scopes.is_empty()
        {
            line.push_str(&format!(
                " ({})",
                tf("missing scopes: {scopes}", &[("scopes", &names(scopes))])
            ));
        }
    }
    line
}

/// What each source's application row could not use, as the catalog reports
/// it.
fn issues(catalog: &Catalog) -> Vec<String> {
    let mut issues = Vec::new();
    for (source, application) in [
        ("user", Some(&catalog.user_application)),
        ("plugin", catalog.plugin_application.as_ref()),
        ("project", catalog.project_application.as_ref()),
    ] {
        let Some(application) = application else {
            continue;
        };
        for error in [
            application.error.as_ref(),
            application.cleanup_error.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            issues.push(format!(
                "{source} {}",
                tf(
                    "MCP configuration: {error}",
                    &[("error", &safe_line(error))]
                )
            ));
        }
    }
    issues
}

/// The document an add starts from: the owner's create shape, filled with
/// example values. The editor has no transport picker, so the document shows
/// the stdio form and the design text says how to switch.
fn create() -> Value {
    json!({
        "id": "example",
        "source": "user",
        "configuration": {
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-everything"],
            "env": {},
            "enabled": false,
        },
    })
}

/// Names the owner reports, joined; `—` when it reports none.
fn names(values: &[String]) -> String {
    if values.is_empty() {
        "—".to_owned()
    } else {
        values
            .iter()
            .map(|value| safe_line(value))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn variables_are_names_only_in_the_detail_lines() {
        let configuration = Configuration::decode(&json!({
            "type": "http",
            "endpoint": "https://example.test",
            "headerNames": ["X-Key"],
            "authConfigured": false,
            "oauthConfigured": false,
            "oauth": null,
            "referencedVariables": ["MCP_HOST", "MCP_TOKEN"],
            "unsetVariables": ["MCP_TOKEN"]
        }))
        .unwrap();
        let lines = configuration.lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Variables") && line.contains("MCP_HOST, MCP_TOKEN"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Unset variables") && line.contains("MCP_TOKEN"))
        );
    }
}
