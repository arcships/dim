//! MCP management wire boundary over the MCP and Settings owners.
use super::auth::{AuthState, McpAuth, McpAuthError, credential_key, stored_key};
use super::config::{McpServerSettings, RemoteTransport};
use super::{McpChange, McpError, McpService, McpSource};
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::{canonical_cwd, optional_non_null},
    settings::{SettingsError, SettingsScope, SettingsService},
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
    methods.register(
        ctx,
        "mcp",
        [
            "mcp.catalog",
            "mcp.temporary.capabilities",
            "mcp.save",
            "mcp.remove",
            "mcp.set_enabled",
            "mcp.disconnect",
            "mcp.auth.start",
            "mcp.auth.complete",
            "mcp.auth.logout",
        ]
        .into_iter()
        .map(|name| MethodDecl::new(name, !matches!(name, "mcp.catalog" | "mcp.temporary.capabilities"), Interruption::Report))
        .collect(),
        Arc::new(move |method, value, stopping, _| {
            let directory = directory.clone();
            let settings = settings.clone();
            let auth = auth.clone();
            Box::pin(async move {
                match method.as_str() {
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
