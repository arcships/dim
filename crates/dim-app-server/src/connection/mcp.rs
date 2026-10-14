//! Local product wire projections over native MCP owners.
use super::*;
use crate::{
    agents::AgentHost,
    mcp::{McpChange, McpError, McpSession, McpSessionState, McpSourceState},
    settings::{McpServerSettings, RemoteTransport, SettingsError, SettingsSource},
};
use dim_agent_capabilities::tool_registry::{ToolSourcesState, ToolSourcesView};
use rutis::{EventKey, FiberState, FiberStatusChanged, Listener};
use tokio::sync::watch;

pub(super) fn error(error: McpError) -> RpcError {
    let kind = match &error {
        McpError::Settings(SettingsError::Conflict) => "mcp_settings_conflict",
        McpError::Settings(SettingsError::Invalid(_))
        | McpError::InvalidSource
        | McpError::InvalidId => "mcp_invalid_config",
        McpError::UnknownServer => "mcp_unknown_server",
        McpError::Closed | McpError::Settings(SettingsError::Closed) => {
            return RpcError::reloading();
        }
        McpError::Cancelled | McpError::Settings(SettingsError::Cancelled) => {
            "operation_interrupted"
        }
        McpError::Settings(_) => "mcp_settings_read",
        _ => "mcp_unavailable",
    };
    // Transport errors already carry fixed diagnostics, without configuration secrets.
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

async fn project(rpc: &RpcService, cwd: Option<String>) -> Result<Option<usize>, RpcError> {
    let Some(cwd) = cwd else { return Ok(None) };
    let cwd = canonical_cwd(&cwd).await?;
    rpc.settings
        .as_ref()
        .expect("declared MCP settings dependency")
        .authorize_project(cwd, &rpc.unloading)
        .await
        .map(Some)
        .map_err(|e| error(e.into()))
}
fn source(scope: McpScope, project: Option<usize>) -> Result<SettingsSource, RpcError> {
    match scope {
        McpScope::User => Ok(SettingsSource::UserMcp),
        McpScope::Project => project
            .map(SettingsSource::ProjectMcp)
            .ok_or_else(|| RpcError::invalid("Project MCP operations require cwd")),
    }
}

pub(super) async fn management(rpc: &RpcService, call: Call) -> Result<Reply, RpcError> {
    let directory = rpc.mcp.as_ref().expect("declared MCP directory dependency");
    let cancel = &rpc.unloading;
    match call {
        Call::McpCatalog(p) => {
            let project = project(rpc, p.cwd).await?;
            let catalog = directory.catalog(project, cancel).await.map_err(error)?;
            Ok(Reply::McpCatalog(McpCatalogResult {
                user_revision: catalog.user_revision,
                project_revision: catalog.project_revision,
                user_application: directory.application(SettingsSource::UserMcp),
                project_application: project
                    .map(|root| directory.application(SettingsSource::ProjectMcp(root))),
                servers: catalog
                    .servers
                    .into_iter()
                    .map(|(id, entry)| McpCatalogEntry {
                        id,
                        source: match entry.source {
                            SettingsSource::UserMcp => McpScope::User,
                            SettingsSource::ProjectMcp(_) => McpScope::Project,
                            SettingsSource::Skills => unreachable!("MCP directory excludes Skills"),
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
            }))
        }
        Call::McpSave(p) => {
            let source = source(p.source, project(rpc, p.cwd).await?)?;
            directory
                .update(
                    source,
                    p.expected_version,
                    p.id,
                    McpChange::Put(p.configuration),
                    cancel,
                )
                .await
                .map_err(error)?;
            Ok(Reply::empty())
        }
        Call::McpRemove(p) => {
            let source = source(p.source, project(rpc, p.cwd).await?)?;
            directory
                .update(source, p.expected_version, p.id, McpChange::Remove, cancel)
                .await
                .map_err(error)?;
            Ok(Reply::empty())
        }
        Call::McpSetEnabled(p) => {
            let source = source(p.source, project(rpc, p.cwd).await?)?;
            directory
                .update(
                    source,
                    p.expected_version,
                    p.id,
                    McpChange::SetEnabled(p.enabled),
                    cancel,
                )
                .await
                .map_err(error)?;
            Ok(Reply::empty())
        }
        Call::McpDisconnect(p) => {
            if p.id.trim().is_empty() {
                return Err(RpcError::invalid("id must not be empty"));
            }
            let source = source(p.source, project(rpc, p.cwd).await?)?;
            directory
                .disconnect(source, p.id, cancel)
                .await
                .map_err(error)?;
            Ok(Reply::empty())
        }
        _ => unreachable!("only MCP management reaches this boundary"),
    }
}

fn project_state(id: &str, state: McpSessionState, tools: ToolSourcesState) -> McpStateResult {
    McpStateResult {
        session_id: id.to_owned(),
        servers: state
            .servers
            .into_iter()
            .map(|(id, state)| {
                let status = tools.0.get(&format!("mcp:{id}"));
                let (state, version, message) = match state {
                    McpSourceState::Disabled => ("disabled", None, None),
                    McpSourceState::Connecting => ("connecting", None, None),
                    McpSourceState::Ready { version } => ("ready", Some(version), None),
                    McpSourceState::Failed { message } => ("failed", None, Some(message)),
                };
                McpServerStatus {
                    id,
                    state,
                    version,
                    message,
                    adopted_version: status.and_then(|s| s.adopted.clone()),
                    rejection: status
                        .and_then(|s| s.rejected.as_ref())
                        .map(|r| McpRejection {
                            version: r.version.clone(),
                            reason: r.reason.clone(),
                        }),
                }
            })
            .collect(),
        catalog_error: state.catalog_error,
        closed: state.closed,
    }
}
pub(super) fn session_state(session: &Session) -> Result<McpStateResult, RpcError> {
    Ok(project_state(
        session.id(),
        session.session_service::<McpSession>()?.state(),
        session.session_service::<ToolSourcesView>()?.get(),
    ))
}

/// Reacquire services when their Plugins become active; never poll readiness.
pub(super) struct Availability(pub(super) watch::Sender<()>);
impl Listener<FiberStatusChanged> for Availability {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        event: &'a FiberStatusChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        if event.to == FiberState::Active {
            self.0.send_replace(());
        }
        Box::pin(async { Ok(None) })
    }
}
impl Connection {
    pub(super) async fn follow_mcp(
        &self,
        host: Arc<AgentHost>,
        session: Session,
        generation: u64,
        cancel: CancellationToken,
    ) {
        let (signal, mut active) = watch::channel(());
        let Ok(listener) = host
            .ctx
            .events()
            .on(&host.ctx, &EventKey::of(), Availability(signal))
        else {
            return;
        };
        let mut owners: Option<(Arc<McpSession>, Arc<ToolSourcesView>)> = None;
        let mut watches = None;
        let mut last = None;
        loop {
            if let (Ok(mcp), Ok(tools)) = (
                session.session_service::<McpSession>(),
                session.session_service::<ToolSourcesView>(),
            ) && !mcp.state().closed
                && owners.as_ref().is_none_or(|(old_mcp, old_tools)| {
                    !Arc::ptr_eq(old_mcp, &mcp) || !Arc::ptr_eq(old_tools, &tools)
                })
            {
                last = None;
                watches = Some((mcp.subscribe(), tools.subscribe()));
                owners = Some((mcp, tools));
            }
            if let Some((mcp, tools)) = &mut watches {
                let state = project_state(
                    session.id(),
                    mcp.borrow_and_update().clone(),
                    tools.borrow_and_update().clone(),
                );
                let closed = state.closed;
                if last.as_ref() != Some(&state) {
                    last = Some(state.clone());
                    if !self.notify(session.id(), generation, Notification::McpState(state)) {
                        break;
                    }
                }
                if closed {
                    watches = None;
                    owners = None;
                    continue;
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = session.closed() => break,
                result = active.changed() => if result.is_err() { break },
                result = async {
                    match &mut watches {
                        Some((mcp, tools)) => tokio::select! { r = mcp.changed() => r, r = tools.changed() => r },
                        None => std::future::pending().await,
                    }
                } => if result.is_err() { watches = None; owners = None; continue; },
            }
        }
        let _ = listener.dispose().await;
    }
}
