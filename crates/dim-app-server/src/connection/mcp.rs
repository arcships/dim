//! Local product wire projections over native MCP owners.
use super::*;
pub(super) use crate::mcp::management::error;
use crate::{
    agents::AgentHost,
    mcp::{McpSession, McpSessionState},
};
use dim_agent_capabilities::tool_registry::{ToolSourcesState, ToolSourcesView};
use rutis::host::{FiberState, FiberStatusChanged};
use tokio::sync::watch;

fn project_state(id: &str, state: McpSessionState, tools: ToolSourcesState) -> McpStateResult {
    McpStateResult {
        session_id: id.to_owned(),
        servers: state
            .servers
            .into_iter()
            .map(|(id, state)| {
                let status = tools.0.get(&format!("mcp:{id}"));
                let (state, version, message) = state.into_wire();
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
/// The refusal vocabulary of `session.mcp.app` (MCP Apps §4.3). A refusal and
/// an unknown outcome are separate codes: only the first may be retried.
pub(super) fn app_error(error: crate::mcp::McpAppError) -> RpcError {
    let kind = match error {
        crate::mcp::McpAppError::Absent => "mcp_app_absent",
        crate::mcp::McpAppError::Unsupported => "mcp_app_unsupported",
        crate::mcp::McpAppError::NotCallable => "mcp_app_not_callable",
        crate::mcp::McpAppError::UnknownServer => "mcp_unknown_server",
        crate::mcp::McpAppError::Unavailable => "mcp_unavailable",
        crate::mcp::McpAppError::Refused => "mcp_app_refused",
        crate::mcp::McpAppError::Unknown => "mcp_app_unknown",
    };
    RpcError::app(-32000, kind, error.to_string())
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
impl Availability {
    pub(super) async fn on_event(
        &self,
        _ctx: &rutis::Ctx,
        event: &FiberStatusChanged,
        _: rutis::events::Call<FiberStatusChanged>,
    ) -> Result<Option<()>, Error> {
        if event.to == FiberState::Active {
            self.0.send_replace(());
        }
        Ok(None)
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
        let Ok(listener) = host.ctx.on::<FiberStatusChanged>({
            let listener = std::sync::Arc::new(Availability(signal));
            move |event, call| {
                let listener = listener.clone();
                async move {
                    let ctx = call.ctx().clone();
                    listener.on_event(&ctx, &event, call).await
                }
            }
        }) else {
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
