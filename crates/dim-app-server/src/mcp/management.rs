//! MCP management wire boundary over the MCP and Settings owners.
use super::auth::{AuthState, McpAuth, McpAuthError, credential_key, stored_key};
use super::config::{BearerAuth, McpServerSettings, McpSettings, RemoteTransport};
use super::{McpChange, McpError, McpService, McpSource};
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::{canonical_cwd, optional_non_null},
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use rutis::{Ctx, Error};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    User,
    Project,
    /// Entries registered by plugins; they change only through their plugin.
    Plugin,
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
    pub configuration: McpServerSettings,
}
impl std::fmt::Debug for SaveMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveMcp").finish_non_exhaustive()
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
    configuration: Option<McpServerSettings>,
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

fn configuration_view(configuration: McpServerSettings) -> Result<serde_json::Value, RpcError> {
    Ok(match configuration {
        McpServerSettings::Stdio {
            command,
            args,
            cwd,
            env,
            ..
        } => serde_json::json!({
            "type": "stdio", "command": command, "argsCount": args.len(), "cwd": cwd,
            "envNames": env.keys().collect::<Vec<_>>()
        }),
        McpServerSettings::Remote {
            kind,
            url,
            headers,
            auth,
            oauth,
            ..
        } => serde_json::json!({
            "type": kind, "endpoint": url::Url::parse(&url)
                .map_err(|_| RpcError::invalid("Invalid MCP endpoint"))?
                .origin().ascii_serialization(),
            "headerNames": headers.keys().collect::<Vec<_>>(),
            "authConfigured": auth.is_some(), "oauthConfigured": oauth.is_some(),
            "oauth": oauth.map(|oauth| serde_json::json!({
                "clientId": oauth.client_id, "callbackPort": oauth.callback_port,
                "scopes": oauth.scopes,
                "metadataConfigured": oauth.auth_server_metadata_url.is_some()
            }))
        }),
    })
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
            ..
        } => {
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
            ..
        } => {
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
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigurationResult {
    /// Absent for a plugin entry, which has no Settings source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub enabled: bool,
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
}

pub(crate) fn error(error: McpError) -> RpcError {
    let kind = match &error {
        McpError::Settings(SettingsError::Conflict) => "mcp_settings_conflict",
        McpError::Settings(SettingsError::Invalid(_)) | McpError::InvalidId => "mcp_invalid_config",
        McpError::UnknownServer => "mcp_unknown_server",
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
            other => other.to_string(),
        },
    )
}

async fn project(
    settings: &Arc<SettingsService>,
    cwd: Option<String>,
    stopping: &CancellationToken,
) -> Result<Option<usize>, RpcError> {
    let Some(cwd) = cwd else { return Ok(None) };
    let cwd = canonical_cwd(&cwd).await?;
    settings
        .authorize_project(cwd, stopping)
        .await
        .map(Some)
        .map_err(|e| error(e.into()))
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

/// The Settings source a write goes to; plugin entries are not written here.
fn writable(source: McpSource) -> Result<SettingsScope, RpcError> {
    source
        .scope()
        .ok_or_else(|| RpcError::invalid("plugin MCP entries change through their plugin"))
}

/// One source's entry, for the `mcp.auth.*` targets.
async fn target(
    directory: &Arc<McpService>,
    source: McpSource,
    id: String,
    cancel: &CancellationToken,
) -> Result<super::McpServerEntry, RpcError> {
    if id.trim().is_empty() {
        return Err(RpcError::invalid("id must not be empty"));
    }
    directory.entry(source, id, cancel).await.map_err(error)
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
            "mcp.disconnect",
            "mcp.auth.start",
            "mcp.auth.complete",
            "mcp.auth.cancel",
            "mcp.auth.logout",
        ]
        .into_iter()
        .map(|name| MethodDecl::new(name, !matches!(name, "mcp.catalog" | "mcp.configuration" | "mcp.inspect" | "mcp.temporary.capabilities"), Interruption::Report))
        .collect(),
        Arc::new(move |method, value, stopping, _| {
            let directory = directory.clone();
            let settings = settings.clone();
            let auth = auth.clone();
            let context = context.clone();
            Box::pin(async move {
                match method.as_str() {
                    "mcp.inspect" => {
                        let p: McpTarget = params(value)?;
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let entry = target(&directory, source, p.id.clone(), &stopping).await?;
                        let connections = directory.publication.inspect(source, p.id, &stopping).await.map_err(error)?;
                        return Ok(serde_json::json!({
                            "enabled": entry.enabled(),
                            "configuration": configuration_view(entry.configuration)?,
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
                            configuration
                        } else if let Some(patch) = p.patch {
                            let scope = writable(source)?;
                            let mut entry = directory.editable(source, p.id.clone(), &stopping).await.map_err(error)?;
                            if !matches!(source, McpSource::User) && !entry.enabled {
                                return Err(error(McpError::Disabled));
                            }
                            patch_configuration(&mut entry.configuration, patch)?;
                            directory.effective(scope, p.id.clone(), entry.configuration, &stopping).await.map_err(error)?
                        } else {
                            let entry = target(&directory, source, p.id.clone(), &stopping).await?;
                            if !matches!(source, McpSource::User) && !entry.enabled() {
                                return Err(error(McpError::Disabled));
                            }
                            entry.configuration
                        };
                        let mut document = McpSettings::default();
                        document.mcp_servers.insert(p.id.clone(), configuration);
                        document.validate().map_err(|e| error(e.into()))?;
                        let configuration = document.mcp_servers.remove(&p.id).unwrap();
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
                            configuration: configuration_view(entry.configuration)?,
                        });
                    }
                    "mcp.update" => {
                        let p: UpdateMcp = params(value)?;
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let scope = writable(source)?;
                        let mut entry = directory.editable(source, p.id.clone(), &stopping).await.map_err(error)?;
                        patch_configuration(&mut entry.configuration, p.patch)?;
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
                                    auth: credential_key(&id, &entry.configuration)
                                        .and_then(|(key, _)| states.get(&key))
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
                                    source: match entry.source {
                                        McpSource::User => McpScope::User,
                                        McpSource::Plugins => McpScope::Plugin,
                                        McpSource::Project(_) => McpScope::Project,
                                    },
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
                                })
                                .collect(),
                        });
                    }
                    "mcp.save" => {
                        let p: SaveMcp = params(value)?;
                        let source = writable(source(
                            p.source,
                            project(&settings, p.cwd, &stopping).await?,
                        )?)?;
                        directory
                            .update(
                                source,
                                p.expected_version,
                                p.id,
                                McpChange::Put(p.configuration),
                                &stopping,
                            )
                            .await
                            .map_err(error)?;
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
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let entry = target(&directory, source, p.id.clone(), &stopping).await?;
                        let (key, server) = credential_key(&p.id, &entry.configuration)
                            .ok_or_else(|| error(McpAuthError::Unsupported.into()))?;
                        // Sign-in contacts the server: only for an entry the
                        // user enabled (ADR 003 D3).
                        if !entry.enabled() {
                            return Err(error(McpError::Disabled));
                        }
                        let McpServerSettings::Remote { oauth, .. } = &entry.configuration else {
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
                        let source = source(p.source, project(&settings, p.cwd, &stopping).await?)?;
                        let entry = target(&directory, source, p.id.clone(), &stopping).await?;
                        let key = stored_key(&p.id, &entry.configuration)
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
