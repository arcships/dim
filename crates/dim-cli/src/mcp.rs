//! The MCP protocol both clients share: catalog and Session state reports,
//! sign-in targets and replies, error hints, and the loopback listener that
//! catches a browser callback. Commands that print or prompt are in
//! `mcp_command` (line client) and `session_host` (TUI).

use crate::{Result, args::Options, client::Client, sanitize::safe_line};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub user_revision: String,
    pub project_revision: Option<String>,
    pub servers: Vec<Server>,
    pub user_application: Application,
    /// The plugin source's application row; a daemon older than that row
    /// reports none.
    #[serde(default)]
    pub plugin_application: Option<Application>,
    pub project_application: Option<Application>,
}
#[derive(Deserialize)]
pub struct Server {
    pub id: String,
    /// `user`, `project` or `plugin`.
    pub source: String,
    pub enabled: bool,
    pub transport: String,
    pub auth: Option<Auth>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    /// `required`, `authorized` or `expired`.
    pub status: String,
    pub missing_scopes: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Application {
    pub error: Option<String>,
    pub cleanup_error: Option<String>,
}

/// What `mcp list` prints: the rows on stdout, then the notes on stderr.
pub async fn catalog_report(
    client: &mut Client,
    options: &Options,
) -> Result<(Vec<String>, Vec<String>)> {
    let catalog: Catalog = client
        .typed("mcp.catalog", json!({"cwd": options.workspace().as_str()}))
        .await?;
    let mut rows = Vec::new();
    for server in catalog.servers {
        rows.push(format!(
            "{:?}\t{}\t{}\t{}\t{}",
            server.id,
            safe_line(&server.source),
            safe_line(&server.transport),
            if server.enabled {
                "enabled"
            } else {
                "disabled"
            },
            safe_line(
                match server.auth.as_ref().map(|auth| auth.status.as_str()) {
                    Some("required") => "sign-in required",
                    Some("authorized") => "signed in",
                    Some("expired") => "sign-in expired",
                    Some(other) => other,
                    None => "—",
                }
            )
        ));
        if let Some(scopes) = server.auth.and_then(|auth| auth.missing_scopes) {
            rows.push(format!(
                "  Missing scopes: {}",
                safe_line(&scopes.join(", "))
            ));
        }
    }
    // The columns describe rows: with no server, the report says so instead.
    let mut notes = if rows.is_empty() {
        vec!["No MCP server is configured.".to_owned()]
    } else {
        vec![
            "Columns: ID, source, transport, enablement, authorization.\nAn enabled entry runs its command or endpoint. Project entries start disabled until you run mcp enable ID.\nSign in with mcp login ID; sign out with mcp logout ID.".to_owned(),
        ]
    };
    for (source, application) in [
        ("user", Some(catalog.user_application)),
        ("plugin", catalog.plugin_application),
        ("project", catalog.project_application),
    ] {
        if let Some(application) = application {
            for error in [application.error, application.cleanup_error]
                .into_iter()
                .flatten()
            {
                notes.push(format!("{source} MCP configuration: {}", safe_line(&error)));
            }
        }
    }
    Ok((rows, notes))
}

pub async fn target(client: &mut Client, options: &Options, id: &str) -> Result<Value> {
    let catalog: Catalog = client
        .typed("mcp.catalog", json!({"cwd": options.workspace().as_str()}))
        .await?;
    let server = catalog
        .servers
        .into_iter()
        .find(|server| server.id == id)
        .ok_or("No matching MCP server; inspect mcp list")?;
    Ok(json!({"id":id,"source":server.source,"cwd":options.workspace().as_str()}))
}

pub fn error(error: crate::Error) -> crate::Error {
    let code = error
        .downcast_ref::<crate::client::RpcError>()
        .and_then(|error| error.data.as_ref())
        .and_then(|data| data.get("appError"))
        .and_then(Value::as_str);
    let action = match code {
        Some("mcp_auth_unsupported") => {
            "OAuth is unavailable for this entry; check its transport, static credentials and store format"
        }
        Some("mcp_auth_discovery_failed") => {
            "Check the server metadata or set oauth.authServerMetadataUrl"
        }
        Some("mcp_auth_client_required") => {
            "Set oauth.clientId; after an issuer change, sign out first"
        }
        Some("mcp_auth_attempt_closed") => {
            "Sign-in expired or was replaced; run mcp login ID again"
        }
        Some("mcp_auth_callback_invalid") => "Paste the full callback URL for this sign-in attempt",
        Some("mcp_auth_failed") => {
            "Authorization was refused; fix the cause and run mcp login ID again"
        }
        Some("mcp_auth_unavailable") => {
            "Authorization service or credential storage is unavailable; retry"
        }
        Some("mcp_unavailable") => {
            "Enable the MCP entry and retry; inspect mcp list for configuration errors"
        }
        _ => return error,
    };
    action.into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    pub attempt_id: String,
    pub authorization_url: String,
    pub redirect_uri: String,
    pub resource: String,
    pub issuer: String,
    /// Unix milliseconds.
    pub expires_at: u64,
}

/// Each server's state in the Session, with the reason under it: a failure's
/// message, a refused tool-source version, or the catalog error for the
/// whole Session.
pub async fn session_report(client: &mut Client, session: &str) -> Result<Vec<String>> {
    let state: State = client
        .typed("session.mcp", json!({"sessionId":session}))
        .await?;
    let mut lines = Vec::new();
    for server in state.servers {
        lines.push(format!("{:?}\t{}", server.id, safe_line(&server.state)));
        if server.state == "auth_required" {
            lines.push(format!(
                "  Sign in with mcp login {:?} or /mcp login {:?}.",
                server.id, server.id
            ));
        } else if let Some(message) = &server.message {
            // `auth_required` says the same thing through its hint above.
            lines.push(format!("  {}", safe_line(message)));
        }
        if let Some(rejection) = &server.rejection {
            lines.push(format!(
                "  Version {} rejected: {}",
                safe_line(&rejection.version),
                safe_line(&rejection.reason)
            ));
        }
    }
    if let Some(error) = &state.catalog_error {
        lines.push(format!("MCP catalog: {}", safe_line(error)));
    }
    Ok(lines)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub session_id: String,
    pub servers: Vec<Status>,
    #[serde(default)]
    pub catalog_error: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub id: String,
    pub state: String,
    /// Why the server failed, from `session.mcp`.
    #[serde(default)]
    pub message: Option<String>,
    /// The last tool-source version the Session refused.
    #[serde(default)]
    pub rejection: Option<Rejection>,
}
#[derive(Deserialize)]
pub struct Rejection {
    pub version: String,
    pub reason: String,
}

pub use dim_client::callback::Capture;
