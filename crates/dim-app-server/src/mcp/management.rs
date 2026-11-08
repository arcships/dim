//! MCP management wire boundary over the MCP and Settings owners.
use super::auth::{AuthState, McpAuth, McpAuthError, credential_key, stored_key};
use super::config::{
    self, BearerAuth, McpSecretEntry, McpServerSettings, McpSettings, RemoteTransport,
};
use super::discovery::{self, StoreId};
use super::import::{self, ImportResolution};
use super::{McpChange, McpError, McpService, McpSource};
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::{canonical_cwd, optional_non_null},
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use rutis::{Ctx, Error};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    User,
    Project,
    /// Entries registered by plugins; they change only through their plugin.
    Plugin,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WritableScope {
    User,
    Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub from: WritableScope,
    pub to: WritableScope,
    pub expected_from_version: String,
    pub expected_to_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTarget {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetMcpEnabled {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
    pub enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
    pub configuration: serde_json::Value,
}
impl std::fmt::Debug for SaveMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveMcp").finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewImportMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
    source: WritableScope,
    document: Option<String>,
    from: Option<String>,
}
impl std::fmt::Debug for PreviewImportMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewImportMcp").finish_non_exhaustive()
    }
}

/// `mcp.import.sources`: the known stores, with no document.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportSourcesMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyImportMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
    source: WritableScope,
    document: Option<String>,
    from: Option<String>,
    expected_version: String,
    expected_source_revision: Option<String>,
    #[serde(default)]
    resolutions: Vec<ImportResolution>,
}
impl std::fmt::Debug for ApplyImportMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyImportMcp").finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
    id: String,
    source: McpScope,
    expected_version: String,
    patch: ConfigurationPatch,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
    id: String,
    source: McpScope,
    #[serde(default, deserialize_with = "optional_non_null")]
    configuration: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "optional_non_null")]
    patch: Option<ConfigurationPatch>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigurationPatch {
    #[serde(default, deserialize_with = "optional_non_null")]
    command: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    args: Option<Vec<String>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    url: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    env: Option<std::collections::BTreeMap<String, Option<String>>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    headers: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// A present `null` clears the entry's limit.
    #[serde(default, deserialize_with = "nullable_patch")]
    timeout: Option<Option<u64>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    bearer: Option<BearerPatch>,
    #[serde(default, deserialize_with = "optional_non_null")]
    oauth: Option<OAuthPatch>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum BearerPatch {
    Set { token: String },
    Remove,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum OAuthPatch {
    Set {
        #[serde(rename = "clientId", default, deserialize_with = "nullable_patch")]
        client_id: Option<Option<String>>,
        #[serde(rename = "callbackPort", default, deserialize_with = "nullable_patch")]
        callback_port: Option<Option<u16>>,
        #[serde(
            rename = "authServerMetadataUrl",
            default,
            deserialize_with = "nullable_patch"
        )]
        auth_server_metadata_url: Option<Option<String>>,
        #[serde(default, deserialize_with = "nullable_patch")]
        scopes: Option<Option<String>>,
    },
    Remove,
}

fn nullable_patch<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn patch_sets_secret(patch: &ConfigurationPatch, secret: &McpSecretEntry) -> bool {
    let static_credential = secret.auth.is_some()
        || secret.headers.as_ref().is_some_and(|headers| {
            headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
        });
    patch.args.is_some()
        || patch.env.is_some()
        || patch.headers.is_some()
        || patch.bearer.is_some()
        || patch.url.is_some()
        || (patch.oauth.is_some() && static_credential)
        || matches!(&patch.oauth, Some(OAuthPatch::Remove) if secret.oauth.is_some())
        || matches!(
            &patch.oauth,
            Some(OAuthPatch::Set {
                auth_server_metadata_url: Some(_),
                ..
            }) if secret
                .oauth
                .as_ref()
                .is_some_and(|oauth| oauth.auth_server_metadata_url.is_some())
        )
}

/// A view shows `timeout` only when the entry states one.
fn with_timeout(view: &mut serde_json::Value, timeout: Option<u64>) {
    if let Some(timeout) = timeout {
        view["timeout"] = timeout.into();
    }
}

fn configuration_view(
    configuration: McpServerSettings,
    referenced: &BTreeSet<String>,
    unset: &BTreeSet<String>,
) -> Result<serde_json::Value, RpcError> {
    match configuration {
        McpServerSettings::Stdio {
            command,
            args,
            cwd,
            env,
            timeout,
            ..
        } => {
            let mut view = serde_json::json!({
                "type": "stdio", "command": command, "argsCount": args.len(), "cwd": cwd,
                "envNames": env.keys().collect::<Vec<_>>(),
                "referencedVariables": referenced, "unsetVariables": unset
            });
            with_timeout(&mut view, timeout);
            Ok(view)
        }
        McpServerSettings::Remote {
            kind,
            url,
            headers,
            auth,
            oauth,
            timeout,
            ..
        } => {
            let endpoint = config::public_endpoint(&url)
                .map_err(|_| RpcError::invalid("Invalid MCP endpoint"))?;
            let mut view = serde_json::json!({
                "type": kind, "endpoint": endpoint,
                "headerNames": headers.keys().collect::<Vec<_>>(),
                "authConfigured": auth.is_some(), "oauthConfigured": oauth.is_some(),
                "referencedVariables": referenced, "unsetVariables": unset,
                "oauth": oauth.map(|oauth| serde_json::json!({
                    "clientId": oauth.client_id, "callbackPort": oauth.callback_port,
                    "scopes": oauth.scopes,
                    "metadataConfigured": oauth.auth_server_metadata_url.is_some()
                }))
            });
            with_timeout(&mut view, timeout);
            Ok(view)
        }
    }
}

fn patch_configuration(
    configuration: &mut McpServerSettings,
    patch: ConfigurationPatch,
) -> Result<(), RpcError> {
    let (changes, secrets) = match configuration {
        McpServerSettings::Stdio {
            command,
            args,
            cwd,
            env,
            timeout,
            ..
        } => {
            if let Some(value) = patch.timeout {
                *timeout = value;
            }
            if patch.url.is_some()
                || patch.headers.is_some()
                || patch.bearer.is_some()
                || patch.oauth.is_some()
            {
                return Err(RpcError::invalid(
                    "remote fields cannot update a stdio server",
                ));
            }
            if let Some(value) = patch.command {
                *command = value;
            }
            if let Some(value) = patch.args {
                *args = value;
            }
            if let Some(value) = patch.cwd {
                *cwd = (!value.is_empty()).then_some(value);
            }
            (patch.env, env)
        }
        McpServerSettings::Remote {
            url,
            headers,
            auth,
            oauth,
            timeout,
            ..
        } => {
            if let Some(value) = patch.timeout {
                *timeout = value;
            }
            if patch.command.is_some()
                || patch.args.is_some()
                || patch.cwd.is_some()
                || patch.env.is_some()
            {
                return Err(RpcError::invalid(
                    "stdio fields cannot update a remote server",
                ));
            }
            if let Some(value) = patch.url {
                *url = value;
            }
            if let Some(bearer) = patch.bearer {
                *auth = match bearer {
                    BearerPatch::Set { token } => Some(BearerAuth::Bearer { token }),
                    BearerPatch::Remove => None,
                };
            }
            if let Some(change) = patch.oauth {
                match change {
                    OAuthPatch::Remove => *oauth = None,
                    OAuthPatch::Set {
                        client_id,
                        callback_port,
                        auth_server_metadata_url,
                        scopes,
                    } => {
                        let oauth = oauth.get_or_insert_with(Default::default);
                        if let Some(value) = client_id {
                            oauth.client_id = value;
                        }
                        if let Some(value) = callback_port {
                            oauth.callback_port = value;
                        }
                        if let Some(value) = auth_server_metadata_url {
                            oauth.auth_server_metadata_url = value;
                        }
                        if let Some(value) = scopes {
                            oauth.scopes = value;
                        }
                    }
                }
            }
            (patch.headers, headers)
        }
    };
    for (name, value) in changes.into_iter().flatten() {
        if name.trim().is_empty() {
            return Err(RpcError::invalid("secret field names must not be empty"));
        }
        match value {
            Some(value) => {
                secrets.insert(name, value);
            }
            None => {
                secrets.remove(&name);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartMcpAuth {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    /// A confidential client's secret, sent once; never returned or logged.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub client_secret: Option<String>,
}
impl std::fmt::Debug for StartMcpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartMcpAuth").finish_non_exhaustive()
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteMcpAuth {
    pub attempt_id: String,
    /// Carries the authorization code; never logged.
    pub callback_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelMcpAuth {
    attempt_id: String,
}

impl std::fmt::Debug for CompleteMcpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompleteMcpAuth").finish_non_exhaustive()
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthLoggedOut {
    pub revoked: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthStatus {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_scopes: Option<Vec<String>>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogResult {
    pub user_revision: String,
    pub project_revision: Option<String>,
    pub user_application: crate::mcp::McpApplication,
    pub plugin_application: crate::mcp::McpApplication,
    pub project_application: Option<crate::mcp::McpApplication>,
    pub servers: Vec<McpCatalogEntry>,
    /// Entries that did not decode, listed apart from `servers` so a client
    /// that knows only `servers` still reads every valid entry.
    pub invalid: Vec<McpInvalidCatalogEntry>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInvalidCatalogEntry {
    pub id: String,
    pub source: McpScope,
    pub error: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigurationResult {
    /// Absent for a plugin entry, which has no Settings source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub enabled: bool,
    pub secrets_in_user_scope: bool,
    pub configuration: serde_json::Value,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogEntry {
    pub id: String,
    pub source: McpScope,
    pub enabled: bool,
    pub transport: &'static str,
    /// Absent for stdio and static-credential entries, and while Dim has
    /// neither a credential nor a challenge for the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuthStatus>,
    /// Names only, sorted and deduplicated.
    pub referenced_variables: BTreeSet<String>,
    /// Names only, sorted and deduplicated.
    pub unset_variables: BTreeSet<String>,
}

pub(crate) fn error(error: McpError) -> RpcError {
    let kind = match &error {
        McpError::Settings(SettingsError::Conflict) => "mcp_settings_conflict",
        McpError::Settings(SettingsError::Invalid(_))
        | McpError::Settings(SettingsError::Document(_))
        | McpError::Settings(SettingsError::DocumentShape { .. })
        | McpError::InvalidId
        | McpError::InvalidEntry(_) => "mcp_invalid_config",
        McpError::UnknownServer => "mcp_unknown_server",
        McpError::MoveConflict => "mcp_move_conflict",
        McpError::MovePartial { .. } => "mcp_move_partial",
        McpError::CredentialsAreUserScoped => "mcp_credentials_are_user_scoped",
        McpError::StaleSecretRecord => "mcp_stale_secret_record",
        McpError::ImportConflict { .. } => "mcp_import_conflict",
        McpError::ImportPartial => "mcp_import_partial",
        McpError::Closed | McpError::Settings(SettingsError::Closed) => {
            return RpcError::reloading();
        }
        McpError::Cancelled | McpError::Settings(SettingsError::Cancelled) => {
            "operation_interrupted"
        }
        McpError::Settings(_) => "mcp_settings_read",
        McpError::Auth(McpAuthError::Closed) => return RpcError::reloading(),
        McpError::Auth(error) => error.code(),
        _ => "mcp_unavailable",
    };
    RpcError::app(
        -32000,
        kind,
        match error {
            McpError::Worker(_) | McpError::Settings(SettingsError::Worker(_)) => {
                "MCP worker failed".into()
            }
            McpError::MoveConflict => "MCP destination source already has this entry".into(),
            McpError::MovePartial { into_project: true } => {
                "MCP move is partial: remove the destination entry and retry".into()
            }
            McpError::MovePartial { into_project: false } => {
                "MCP move is partial: remove the project source entry".into()
            }
            McpError::CredentialsAreUserScoped => "MCP secret fields are stored in user scope; call mcp.move {id, from: 'project', to: 'user', cwd, expectedFromVersion, expectedToVersion} to edit them".into(),
            McpError::StaleSecretRecord => "MCP withheld record remains for this id without a project entry; call mcp.remove {source: 'project', id, cwd, expectedVersion} to delete it (it answers mcp_unknown_server after cleaning), then save again".into(),
            McpError::ImportPartial => "MCP import is partial: the source entries were written and their withheld values were not; replace them again to retry".into(),
            other => other.to_string(),
        },
    )
}

/// The authorized workspace of a request's `cwd`: its Settings root, its
/// canonical path and the string the request named it by. Claude Code records
/// its local store under either spelling, so both are carried.
async fn authorized_workspace(
    settings: &Arc<SettingsService>,
    cwd: Option<String>,
    stopping: &CancellationToken,
) -> Result<Option<(usize, discovery::Workspace)>, RpcError> {
    let Some(cwd) = cwd else { return Ok(None) };
    let canonical = canonical_cwd(&cwd).await?;
    let root = settings
        .authorize_project(canonical.clone(), stopping)
        .await
        .map_err(|e| error(e.into()))?;
    Ok(Some((
        root,
        discovery::Workspace {
            canonical,
            request: cwd,
        },
    )))
}

async fn project(
    settings: &Arc<SettingsService>,
    cwd: Option<String>,
    stopping: &CancellationToken,
) -> Result<Option<usize>, RpcError> {
    Ok(authorized_workspace(settings, cwd, stopping)
        .await?
        .map(|(root, _)| root))
}

/// The document an import request names: the client's own text, or one known
/// store's converted text. A store document answers its digest, so an apply
/// can require the text the preview read. Nothing is written here.
async fn import_document(
    document: Option<String>,
    from: Option<String>,
    workspace: Option<&(usize, discovery::Workspace)>,
    stopping: &CancellationToken,
) -> Result<(String, Option<String>, Option<discovery::Identity>), RpcError> {
    match (document, from) {
        (Some(document), None) => Ok((document, None, None)),
        (None, Some(id)) => {
            let store = StoreId::parse(&id)
                .ok_or_else(|| RpcError::invalid(format!("unknown MCP source {id}")))?;
            if store.requires_workspace() && workspace.is_none() {
                return Err(RpcError::invalid("Project MCP sources require cwd"));
            }
            let workspace = workspace.map(|(_, workspace)| workspace.clone());
            let read = tokio::task::spawn_blocking(move || {
                let environment = discovery::Environment::from_process();
                // The identity row comes from the one read of the store the
                // preview converts, so naming it reads nothing else.
                discovery::document(store, &environment, workspace.as_ref())
            })
            .await
            .map_err(|_| RpcError::app(-32000, "mcp_unavailable", "MCP worker failed"))?;
            let (document, source) = read.map_err(|error| {
                let code = error.code();
                RpcError::app(-32000, code, error.message)
            })?;
            if stopping.is_cancelled() {
                return Err(RpcError::interrupted());
            }
            Ok((document.text, Some(document.revision), Some(source)))
        }
        _ => Err(RpcError::invalid(
            "mcp.import takes exactly one of document and from",
        )),
    }
}

fn source(scope: McpScope, project: Option<usize>) -> Result<McpSource, RpcError> {
    match scope {
        McpScope::User => Ok(McpSource::User),
        McpScope::Plugin => Ok(McpSource::Plugins),
        McpScope::Project => project
            .map(McpSource::Project)
            .ok_or_else(|| RpcError::invalid("Project MCP operations require cwd")),
    }
}

fn scope(source: McpSource) -> McpScope {
    match source {
        McpSource::User => McpScope::User,
        McpSource::Plugins => McpScope::Plugin,
        McpSource::Project(_) => McpScope::Project,
    }
}

/// The Settings source a write goes to; plugin entries are not written here.
fn writable(source: McpSource) -> Result<SettingsScope, RpcError> {
    source
        .scope()
        .ok_or_else(|| RpcError::invalid("plugin MCP entries change through their plugin"))
}

/// The source an import writes: the user source or an authorized workspace.
fn writable_scope(scope: WritableScope, project: Option<usize>) -> Result<SettingsScope, RpcError> {
    match scope {
        WritableScope::User => Ok(SettingsScope::User),
        WritableScope::Project => project
            .map(SettingsScope::Project)
            .ok_or_else(|| RpcError::invalid("Project MCP operations require cwd")),
    }
}

pub(super) fn provide(
    ctx: &Ctx,
    methods: &ManagementMethods,
    directory: Arc<McpService>,
    settings: Arc<SettingsService>,
    auth: Arc<McpAuth>,
) -> Result<(), Error> {
    let context = ctx.clone();
    methods.register(
        ctx,
        "mcp",
        [
            "mcp.catalog",
            "mcp.configuration",
            "mcp.inspect",
            "mcp.test",
            "mcp.update",
            "mcp.temporary.capabilities",
            "mcp.save",
            "mcp.remove",
            "mcp.set_enabled",
            "mcp.move",
            "mcp.import.sources",
            "mcp.import.preview",
            "mcp.import.apply",
            "mcp.disconnect",
            "mcp.auth.start",
            "mcp.auth.complete",
            "mcp.auth.cancel",
            "mcp.auth.logout",
        ]
        .into_iter()
        .map(|name| MethodDecl::new(name, !matches!(name, "mcp.catalog" | "mcp.configuration" | "mcp.inspect" | "mcp.temporary.capabilities" | "mcp.import.preview" | "mcp.import.sources"), Interruption::Report))
        .collect(),
        Arc::new(move |call, stopping, _| {
            let MethodCall { method, params: value, .. } = call;
            let directory = directory.clone();
            let settings = settings.clone();
            let auth = auth.clone();
            let context = context.clone();
            Box::pin(async move {
                match method.as_str() {
                    "mcp.inspect" => {
                        let p: McpTarget = params(value)?;
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let resolved = directory.resolved(source, p.id.clone(), &stopping).await.map_err(error)?;
                        let entry = resolved.entry;
                        let connections = directory.publication.inspect(source, p.id, &stopping).await.map_err(error)?;
                        return Ok(serde_json::json!({
                            "enabled": entry.enabled(),
                            "configuration": configuration_view(
                                entry.configuration,
                                &entry.referenced_variables,
                                &entry.unset_variables,
                            )?,
                            "connections": connections
                        }));
                    }
                    "mcp.test" => {
                        let p: TestMcp = params(value)?;
                        if p.id.trim().is_empty() {
                            return Err(RpcError::invalid("id must not be empty"));
                        }
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let configuration = if let Some(configuration) = p.configuration {
                            if !matches!(source, McpSource::User) || p.patch.is_some() {
                                return Err(RpcError::invalid("A new test configuration must be user-supplied and cannot include a patch"));
                            }
                            let configuration = config::decode_entry(&p.id, &configuration)
                                .map_err(|entry| error(McpError::InvalidEntry(entry)))?;
                            directory
                                .connected(
                                    SettingsScope::User,
                                    p.id.clone(),
                                    configuration,
                                    None,
                                    &stopping,
                                )
                                .await
                                .map_err(error)?
                                .configuration
                        } else if let Some(patch) = p.patch {
                            let scope = writable(source)?;
                            let mut entry = directory.editable(source, p.id.clone(), &stopping).await.map_err(error)?;
                            if !matches!(source, McpSource::User) && !entry.enabled {
                                return Err(error(McpError::Disabled));
                            }
                            // The members this patch sets are the user's own
                            // text; the rest of a workspace entry is guarded.
                            let mut overlay = entry.secret.clone().unwrap_or_default();
                            if patch.url.is_some() {
                                overlay.url = Some(String::new());
                            }
                            if let Some(headers) = &patch.headers {
                                let user = overlay.headers.get_or_insert_with(Default::default);
                                for (name, value) in headers {
                                    if value.is_some() {
                                        user.insert(name.clone(), String::new());
                                    }
                                }
                            }
                            if matches!(&patch.bearer, Some(BearerPatch::Set { .. })) {
                                overlay.auth = Some(BearerAuth::Bearer {
                                    token: String::new(),
                                });
                            }
                            patch_configuration(&mut entry.configuration, patch)?;
                            let configuration = entry
                                .configuration
                                .canonical(&p.id)
                                .map_err(|entry| error(McpError::InvalidEntry(entry)))?;
                            let expansion = directory
                                .connected(
                                    scope,
                                    p.id.clone(),
                                    configuration,
                                    Some(overlay),
                                    &stopping,
                                )
                                .await
                                .map_err(error)?;
                            expansion.configuration
                        } else {
                            let resolved = directory
                                .resolved(source, p.id.clone(), &stopping)
                                .await
                                .map_err(error)?;
                            if !matches!(source, McpSource::User) && !resolved.entry.enabled() {
                                return Err(error(McpError::Disabled));
                            }
                            resolved.expanded
                        };
                        let mut document = McpSettings::default();
                        document.insert(p.id.clone(), configuration);
                        document.validate().map_err(|e| error(e.into()))?;
                        let configuration = document.remove(&p.id).unwrap();
                        let probe = directory.test(context, p.id, configuration, &stopping).await.map_err(error)?;
                        return reply(probe);
                    }
                    "mcp.configuration" => {
                        let p: McpTarget = params(value)?;
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let entry = directory.editable(source, p.id, &stopping).await.map_err(error)?;
                        return reply(McpConfigurationResult {
                            revision: entry.revision,
                            enabled: entry.enabled,
                            secrets_in_user_scope: entry.secrets_in_user_scope,
                            configuration: configuration_view(
                                entry.configuration,
                                &entry.referenced_variables,
                                &entry.unset_variables,
                            )?,
                        });
                    }
                    "mcp.update" => {
                        let p: UpdateMcp = params(value)?;
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let scope = writable(source)?;
                        let mut entry = directory.editable(source, p.id.clone(), &stopping).await.map_err(error)?;
                        if let Some(secret) = &entry.secret
                            && patch_sets_secret(&p.patch, secret)
                        {
                            return Err(error(McpError::CredentialsAreUserScoped));
                        }
                        patch_configuration(&mut entry.configuration, p.patch)?;
                        if let Some(secret) = &entry.secret {
                            config::strip_secret(&mut entry.configuration, secret);
                        }
                        directory.update(scope, p.expected_version, p.id, McpChange::Put(entry.configuration), &stopping).await.map_err(error)?;
                    }
                    "mcp.temporary.capabilities" => return Ok(serde_json::json!({"transports":["stdio","http","sse"],"leaseSeconds":super::temporary::LEASE.as_secs()})),
                    "mcp.catalog" => {
                        let p: SelectionScope = params(value)?;
                        let project = project(&settings, p.cwd, &stopping).await?;
                        let catalog = directory.catalog(project, &stopping).await.map_err(error)?;
                        let states = auth.states(&stopping).await;
                        return reply(McpCatalogResult {
                            user_revision: catalog.user_revision,
                            project_revision: catalog.project_revision,
                            user_application: directory.application(McpSource::User),
                            plugin_application: directory.application(McpSource::Plugins),
                            project_application: project
                                .map(|root| directory.application(McpSource::Project(root))),
                            servers: catalog
                                .servers
                                .into_iter()
                                .map(|(id, entry)| McpCatalogEntry {
                                    auth: entry
                                        .credential
                                        .as_ref()
                                        .and_then(|key| states.get(key))
                                        .map(|state| match state {
                                            AuthState::Required => McpAuthStatus {
                                                status: "required",
                                                missing_scopes: None,
                                            },
                                            AuthState::Expired => McpAuthStatus {
                                                status: "expired",
                                                missing_scopes: None,
                                            },
                                            AuthState::Authorized { missing_scopes } => {
                                                McpAuthStatus {
                                                    status: "authorized",
                                                    missing_scopes: (!missing_scopes.is_empty())
                                                        .then_some(missing_scopes),
                                                }
                                            }
                                        }),
                                    id,
                                    source: scope(entry.source),
                                    enabled: entry.enabled(),
                                    transport: match entry.configuration {
                                        McpServerSettings::Stdio { .. } => "stdio",
                                        McpServerSettings::Remote {
                                            kind: RemoteTransport::Http,
                                            ..
                                        } => "http",
                                        McpServerSettings::Remote {
                                            kind: RemoteTransport::Sse,
                                            ..
                                        } => "sse",
                                    },
                                    referenced_variables: entry.referenced_variables,
                                    unset_variables: entry.unset_variables,
                                })
                                .collect(),
                            invalid: catalog
                                .invalid
                                .into_iter()
                                .map(|entry| McpInvalidCatalogEntry {
                                    id: entry.id,
                                    source: scope(entry.source),
                                    error: entry.error.to_string(),
                                })
                                .collect(),
                        });
                    }
                    "mcp.save" => {
                        let p: SaveMcp = params(value)?;
                        let mcp_source = source(
                            p.source,
                            project(&settings, p.cwd, &stopping).await?,
                        )?;
                        let source = writable(mcp_source)?;
                        let configuration = config::decode_entry(&p.id, &p.configuration)
                            .map_err(|entry| error(McpError::InvalidEntry(entry)))?;
                        // A broken project entry's record cannot apply to the
                        // replacement, so a successful save deletes it at the
                        // generation read here. A record that changed after
                        // that read is kept.
                        let mut replaced_record = None;
                        if matches!(mcp_source, McpSource::Project(_)) {
                            // A save replaces the whole entry, so an overlay
                            // is refused while its entry exists, and a record
                            // whose entry an external edit deleted is refused
                            // rather than merged into the new entry. The save
                            // deletes nothing else: a concurrent move may own
                            // that record, and may have committed the entry
                            // between the reads, so the entry read again after
                            // the record decides the answer. An unreadable
                            // overlay fails the save.
                            match directory
                                .editable(mcp_source, p.id.clone(), &stopping)
                                .await
                            {
                                Ok(entry) if entry.secrets_in_user_scope => {
                                    return Err(error(McpError::CredentialsAreUserScoped));
                                }
                                Ok(_) => {}
                                // A broken entry is replaceable; its kept
                                // record belongs to the entry being replaced.
                                Err(McpError::InvalidEntry(_)) => {
                                    replaced_record = directory
                                        .overlay(source, p.id.clone(), &stopping)
                                        .await
                                        .map_err(error)?
                                        .map(|secret| secret.generation);
                                }
                                Err(McpError::UnknownServer) => {
                                    if directory
                                        .overlay(source, p.id.clone(), &stopping)
                                        .await
                                        .map_err(error)?
                                        .is_some()
                                    {
                                        match directory
                                            .editable(mcp_source, p.id.clone(), &stopping)
                                            .await
                                        {
                                            Ok(entry) if entry.secrets_in_user_scope => {
                                                return Err(error(
                                                    McpError::CredentialsAreUserScoped,
                                                ));
                                            }
                                            Ok(_) => {}
                                            Err(McpError::UnknownServer) => {
                                                return Err(error(McpError::StaleSecretRecord));
                                            }
                                            Err(failure) => return Err(error(failure)),
                                        }
                                    }
                                }
                                Err(failure) => return Err(error(failure)),
                            }
                        }
                        let id = p.id.clone();
                        directory
                            .update(
                                source,
                                p.expected_version,
                                p.id,
                                McpChange::Put(configuration),
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
                        if let Some(generation) = replaced_record {
                            directory
                                .clear_secret_if(source, id, generation, &stopping)
                                .await
                                .map_err(error)?;
                        }
                    }
                    "mcp.remove" => {
                        let p: RemoveMcp = params(value)?;
                        let source = writable(source(
                            p.source,
                            project(&settings, p.cwd, &stopping).await?,
                        )?)?;
                        directory
                            .update(
                                source,
                                p.expected_version,
                                p.id,
                                McpChange::Remove,
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
                    }
                    "mcp.set_enabled" => {
                        let p: SetMcpEnabled = params(value)?;
                        let source = writable(source(
                            p.source,
                            project(&settings, p.cwd, &stopping).await?,
                        )?)?;
                        directory
                            .update(
                                source,
                                p.expected_version,
                                p.id,
                                McpChange::SetEnabled(p.enabled),
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
                    }
                    "mcp.move" => {
                        let p: MoveMcp = params(value)?;
                        if p.id.trim().is_empty() {
                            return Err(RpcError::invalid("id must not be empty"));
                        }
                        let project = project(&settings, p.cwd, &stopping).await?;
                        let (from, to) = match (p.from, p.to) {
                            (WritableScope::User, WritableScope::Project) => (
                                McpSource::User,
                                McpSource::Project(project.ok_or_else(|| {
                                    RpcError::invalid("Project MCP operations require cwd")
                                })?),
                            ),
                            (WritableScope::Project, WritableScope::User) => (
                                McpSource::Project(project.ok_or_else(|| {
                                    RpcError::invalid("Project MCP operations require cwd")
                                })?),
                                McpSource::User,
                            ),
                            _ => {
                                return Err(RpcError::invalid(
                                    "mcp.move requires one user and one project scope",
                                ));
                            }
                        };
                        directory
                            .move_entry(
                                from,
                                to,
                                p.id,
                                p.expected_from_version,
                                p.expected_to_version,
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
                    }
                    "mcp.import.sources" => {
                        let p: ImportSourcesMcp = params(value)?;
                        let workspace =
                            authorized_workspace(&settings, p.cwd, &stopping).await?;
                        let sources = tokio::task::spawn_blocking(move || {
                            let workspace = workspace.map(|(_, workspace)| workspace);
                            discovery::sources(
                                &discovery::Environment::from_process(),
                                workspace.as_ref(),
                            )
                        })
                        .await
                        .map_err(|_| {
                            RpcError::app(-32000, "mcp_unavailable", "MCP worker failed")
                        })?;
                        return Ok(serde_json::json!({"sources": sources}));
                    }
                    "mcp.import.preview" => {
                        let p: PreviewImportMcp = params(value)?;
                        let workspace =
                            authorized_workspace(&settings, p.cwd, &stopping).await?;
                        let source = writable_scope(
                            p.source,
                            workspace.as_ref().map(|(root, _)| *root),
                        )?;
                        let (document, source_revision, store) =
                            import_document(p.document, p.from, workspace.as_ref(), &stopping)
                                .await?;
                        let document = import::parse_document(&document).map_err(error)?;
                        let mut preview = directory
                            .preview_import(source, document, &stopping)
                            .await
                            .map_err(error)?;
                        preview.source_revision = source_revision;
                        preview.store = store;
                        return reply(preview);
                    }
                    "mcp.import.apply" => {
                        let p: ApplyImportMcp = params(value)?;
                        match (&p.from, &p.expected_source_revision) {
                            (Some(_), None) => {
                                return Err(RpcError::invalid(
                                    "an import from a source takes expectedSourceRevision",
                                ));
                            }
                            (None, Some(_)) => {
                                return Err(RpcError::invalid(
                                    "expectedSourceRevision is only for an import from a source",
                                ));
                            }
                            _ => {}
                        }
                        let workspace =
                            authorized_workspace(&settings, p.cwd, &stopping).await?;
                        let source = writable_scope(
                            p.source,
                            workspace.as_ref().map(|(root, _)| *root),
                        )?;
                        let (document, source_revision, _) =
                            import_document(p.document, p.from, workspace.as_ref(), &stopping)
                                .await?;
                        if let (Some(expected), Some(actual)) =
                            (&p.expected_source_revision, &source_revision)
                            && expected != actual
                        {
                            return Err(RpcError::app(
                                -32000,
                                "mcp_settings_conflict",
                                "The MCP source store changed since the preview; preview again",
                            ));
                        }
                        let document = import::parse_document(&document).map_err(error)?;
                        let resolutions = import::check_resolutions(&document, &p.resolutions)
                            .map_err(RpcError::invalid)?;
                        let revision = directory
                            .import_entries(
                                source,
                                p.expected_version,
                                document,
                                resolutions,
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
                        return Ok(serde_json::json!({"revision": revision}));
                    }
                    "mcp.disconnect" => {
                        let p: McpTarget = params(value)?;
                        if p.id.trim().is_empty() {
                            return Err(RpcError::invalid("id must not be empty"));
                        }
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        directory
                            .disconnect(source, p.id, &stopping)
                            .await
                            .map_err(error)?;
                    }
                    "mcp.auth.start" => {
                        let p: StartMcpAuth = params(value)?;
                        if p.id.trim().is_empty() {
                            return Err(RpcError::invalid("id must not be empty"));
                        }
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let resolved = directory
                            .resolved(source, p.id.clone(), &stopping)
                            .await
                            .map_err(error)?;
                        let (key, server) = credential_key(&p.id, &resolved.expanded)
                            .ok_or_else(|| error(McpAuthError::Unsupported.into()))?;
                        // Sign-in contacts the server: only for an entry the
                        // user enabled (ADR 003 D3).
                        if !resolved.entry.enabled() {
                            return Err(error(McpError::Disabled));
                        }
                        let McpServerSettings::Remote { oauth, .. } = &resolved.expanded else {
                            unreachable!("OAuth entries are remote");
                        };
                        if let Some(secret) = &p.client_secret
                            && (secret.is_empty()
                                || oauth
                                    .as_deref()
                                    .is_none_or(|oauth| oauth.client_id.is_none()))
                        {
                            return Err(RpcError::invalid(
                                "clientSecret requires oauth.clientId and a value",
                            ));
                        }
                        let started = auth
                            .start(&key, &server, oauth.as_deref(), p.client_secret, &stopping)
                            .await
                            .map_err(|e| error(e.into()))?;
                        return reply(started);
                    }
                    "mcp.auth.cancel" => {
                        let p: CancelMcpAuth = params(value)?;
                        let id = uuid::Uuid::parse_str(&p.attempt_id)
                            .map_err(|_| RpcError::invalid("attemptId must be a UUID"))?;
                        auth.cancel_attempt(id, &stopping).await.map_err(|e| error(e.into()))?;
                    }
                    "mcp.auth.complete" => {
                        let p: CompleteMcpAuth = params(value)?;
                        let id = uuid::Uuid::parse_str(&p.attempt_id)
                            .map_err(|_| RpcError::invalid("attemptId must be a UUID"))?;
                        auth.complete(id, &p.callback_url, &stopping)
                            .await
                            .map_err(|e| error(e.into()))?;
                    }
                    "mcp.auth.logout" => {
                        let p: McpTarget = params(value)?;
                        if p.id.trim().is_empty() {
                            return Err(RpcError::invalid("id must not be empty"));
                        }
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let resolved = directory
                            .resolved(source, p.id.clone(), &stopping)
                            .await
                            .map_err(error)?;
                        let key = stored_key(&p.id, &resolved.expanded)
                            .ok_or_else(|| error(McpAuthError::Unsupported.into()))?;
                        let revoked = auth
                            .logout(&key, &stopping)
                            .await
                            .map_err(|e| error(e.into()))?;
                        return reply(McpAuthLoggedOut { revoked });
                    }
                    _ => unreachable!("registered MCP management method"),
                }
                reply(serde_json::json!({}))
            })
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(url: &str) -> McpServerSettings {
        McpServerSettings::Remote {
            kind: RemoteTransport::Http,
            url: url.into(),
            headers: Default::default(),
            auth: None,
            oauth: None,
            enabled: None,
            timeout: None,
            extra: Default::default(),
        }
    }

    /// #952: a malformed endpoint without a reference keeps the view's error
    /// and never reaches the client whole; a reference stays as written and a
    /// parseable URL is reduced to its origin.
    #[test]
    fn a_configuration_view_errs_on_a_malformed_endpoint() {
        let (referenced, unset) = (BTreeSet::new(), BTreeSet::new());
        let error =
            configuration_view(remote("host/mcp?token=made-up-secret"), &referenced, &unset)
                .unwrap_err();
        assert_eq!(error.message, "Invalid MCP endpoint");

        let view = configuration_view(
            remote("https://user:made-up-credential@host/mcp"),
            &referenced,
            &unset,
        )
        .unwrap();
        assert_eq!(view["endpoint"], "https://host");

        let view =
            configuration_view(remote("https://${MCP_HOST}/mcp"), &referenced, &unset).unwrap();
        assert_eq!(view["endpoint"], "https://${MCP_HOST}");
    }
}
