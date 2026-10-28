//! Typed MCP documents and the effective configuration of each entry.
use crate::settings::{BusinessDocument, Result, SettingsError, SettingsScope, SettingsService};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Wakes the returned receiver on every committed write to MCP's Settings
/// sources and on every newly authorized project. The listeners belong to
/// `ctx` and end with it; register before the first read of the sources.
pub(super) fn settings_changes(
    ctx: &rutis::Ctx,
) -> std::result::Result<tokio::sync::watch::Receiver<()>, rutis::Error> {
    let wake = std::sync::Arc::new(tokio::sync::watch::Sender::new(()));
    for source in [McpSettings::NAMESPACE, McpProjectEnablement::NAMESPACE] {
        let wake = wake.clone();
        ctx.listen::<crate::settings::SettingsChanged>()
            .name(crate::settings::settings_changes(source))
            .on(move |_, _| {
                wake.send_replace(());
                async { Ok(None) }
            })?;
    }
    let authorized = wake.clone();
    ctx.listen::<crate::settings::ProjectAuthorized>()
        .name(crate::settings::project_authorizations())
        .on(move |_, _| {
            authorized.send_replace(());
            async { Ok(None) }
        })?;
    Ok(wake.subscribe())
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpSettings {
    pub mcp_servers: BTreeMap<String, McpServerSettings>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerSettings {
    Stdio {
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        kind: Option<Stdio>,
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Remote {
        #[serde(rename = "type")]
        kind: RemoteTransport,
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        auth: Option<BearerAuth>,
        #[serde(skip_serializing_if = "Option::is_none")]
        oauth: Option<Box<McpOAuthSettings>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Stdio {
    Stdio,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteTransport {
    Http,
    Sse,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BearerAuth {
    Bearer { token: String },
}

/// A remote entry's OAuth settings, with the field names and types of
/// Claude Code's `.mcp.json`. They carry no secret.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthSettings {
    /// A pre-registered client (registration priority 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Port of the redirect `http://localhost:{port}/callback`; default 1456.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    /// Authorization server metadata used only when the server publishes no
    /// valid Protected Resource Metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
    /// Space-separated scopes requested as they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl McpOAuthSettings {
    fn validate(&self) -> Result<()> {
        let metadata_url = self
            .auth_server_metadata_url
            .as_deref()
            .map(url::Url::parse);
        if self.client_id.as_ref().is_some_and(|id| id.trim().is_empty())
            || self.callback_port == Some(0)
            || metadata_url.is_some_and(|url| {
                !url.is_ok_and(|url| {
                    url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                        && (url.scheme() == "https"
                            || (url.scheme() == "http"
                                && matches!(url.host(), Some(host) if loopback(&host))))
                })
            })
            // RFC 6749 §3.3 scope tokens, separated by spaces.
            || self.scopes.as_ref().is_some_and(|scopes| {
                scopes.trim().is_empty()
                    || !scopes
                        .bytes()
                        .all(|b| b == b' ' || (b.is_ascii_graphic() && b != b'"' && b != b'\\'))
            })
        {
            return Err(SettingsError::Invalid("invalid MCP OAuth configuration"));
        }
        Ok(())
    }
}

pub(crate) fn loopback(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    }
}

impl BusinessDocument for McpSettings {
    const NAMESPACE: &'static str = "mcp";
    const USER_FILE: &'static str = "mcp.json";
    const PROJECT_FILE: Option<&'static str> = Some(".mcp.json");

    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("mcpServers") {
            return Err(SettingsError::Invalid("reserved MCP configuration field"));
        }
        for (id, server) in &self.mcp_servers {
            if id.trim().is_empty() {
                return Err(SettingsError::Invalid("empty MCP server id"));
            }
            let (target, extra) = match server {
                McpServerSettings::Stdio { command, extra, .. } => (command, extra),
                McpServerSettings::Remote {
                    url,
                    headers,
                    auth,
                    oauth,
                    extra,
                    ..
                } => {
                    if let Some(BearerAuth::Bearer { token }) = auth
                        && token.trim().is_empty()
                    {
                        return Err(SettingsError::Invalid("empty MCP bearer token"));
                    }
                    if let Some(oauth) = oauth {
                        // Static credentials exclude OAuth (design D15).
                        if auth.is_some()
                            || headers
                                .keys()
                                .any(|name| name.eq_ignore_ascii_case("authorization"))
                        {
                            return Err(SettingsError::Invalid(
                                "MCP OAuth excludes static credentials",
                            ));
                        }
                        oauth.validate()?;
                    }
                    (url, extra)
                }
            };
            if target.trim().is_empty()
                || extra.keys().any(|key| {
                    matches!(
                        key.as_str(),
                        "type"
                            | "command"
                            | "args"
                            | "env"
                            | "cwd"
                            | "enabled"
                            | "url"
                            | "headers"
                            | "auth"
                            | "transport"
                            | "oauth"
                    )
                })
            {
                return Err(SettingsError::Invalid(
                    "invalid MCP transport configuration",
                ));
            }
            if extra
                .get("metadata")
                .is_some_and(|value| !value.is_object())
            {
                return Err(SettingsError::Invalid("MCP metadata must be an object"));
            }
        }
        Ok(())
    }
}

/// The user's enablement of project entries (ADR 003 D3), keyed by canonical
/// workspace and server id. A project source starts disabled: its own files
/// cannot enable an entry, and only this user-local record does.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpProjectEnablement {
    #[serde(default)]
    pub enabled: BTreeMap<String, BTreeSet<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl McpProjectEnablement {
    pub fn contains(&self, workspace: &str, id: &str) -> bool {
        self.enabled
            .get(workspace)
            .is_some_and(|ids| ids.contains(id))
    }
    /// Returns whether the record changed.
    pub fn set(&mut self, workspace: &str, id: &str, enabled: bool) -> bool {
        if enabled {
            return self
                .enabled
                .entry(workspace.to_owned())
                .or_default()
                .insert(id.to_owned());
        }
        let Some(ids) = self.enabled.get_mut(workspace) else {
            return false;
        };
        let changed = ids.remove(id);
        if ids.is_empty() {
            self.enabled.remove(workspace);
        }
        changed
    }
}
impl BusinessDocument for McpProjectEnablement {
    const NAMESPACE: &'static str = "mcp-projects";
    const USER_FILE: &'static str = "mcp-projects.json";

    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("enabled")
            || self.enabled.iter().any(|(workspace, ids)| {
                !Path::new(workspace).is_absolute()
                    || ids.is_empty()
                    || ids.iter().any(|id| id.trim().is_empty())
            })
        {
            return Err(SettingsError::Invalid("invalid MCP project enablement"));
        }
        Ok(())
    }
}

/// The canonical workspace of an authorized project source, as the
/// enablement key.
pub(crate) fn workspace(settings: &SettingsService, scope: SettingsScope) -> Result<String> {
    let SettingsScope::Project(root) = scope else {
        return Err(SettingsError::Invalid("not a project MCP source"));
    };
    settings
        .project_path(root)?
        .to_str()
        .map(str::to_owned)
        .ok_or(SettingsError::Invalid("project cwd must be UTF-8"))
}

/// The configuration an entry runs with. A project entry resolves its stdio
/// cwd against the workspace and takes its enablement only from the user's
/// record; its own `enabled` field is ignored.
pub(crate) fn effective(
    settings: &SettingsService,
    scope: SettingsScope,
    id: &str,
    mut config: McpServerSettings,
    enablement: &McpProjectEnablement,
) -> Result<McpServerSettings> {
    if matches!(scope, SettingsScope::Project(_)) {
        let workspace = workspace(settings, scope)?;
        let (McpServerSettings::Stdio { enabled, .. } | McpServerSettings::Remote { enabled, .. }) =
            &mut config;
        *enabled = Some(enablement.contains(&workspace, id));
        if let McpServerSettings::Stdio { cwd, .. } = &mut config {
            let workspace = PathBuf::from(&workspace);
            let path = cwd
                .as_ref()
                .map_or(workspace.clone(), |cwd| workspace.join(cwd));
            *cwd = Some(
                path.to_str()
                    .ok_or(SettingsError::Invalid("project cwd must be UTF-8"))?
                    .to_owned(),
            );
        }
    }
    Ok(config)
}
