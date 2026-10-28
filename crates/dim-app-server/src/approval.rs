//! RPC layer of branch approvals; the capability is
//! `dim_agent_capabilities::approval`.
use crate::protocol::ApprovalDecision;
use dim_agent_capabilities::approval::{Approval, ApprovalRespondError};
use dim_agent_capabilities::methods::RpcError;
use dim_agent_runtime::{AgentError, Session};
use dim_agent_types::ApprovalRequestData;

/// `approval.respond` on the approval capability of `branch_id`.
pub(crate) async fn respond(
    session: &Session,
    branch_id: &str,
    approval_id: &str,
    decision: ApprovalDecision,
) -> Result<(), RpcError> {
    let approval = session
        .service::<Approval>(branch_id)
        .map_err(|error| match error {
            AgentError::UnknownBranch(_) => RpcError::invalid("Approval branch does not exist"),
            AgentError::BranchEnded(_) => RpcError::invalid("Approval branch has ended"),
            error => RpcError::from(error),
        })?;
    approval
        .respond(approval_id, decision)
        .await
        .map_err(|error| match error {
            ApprovalRespondError::Unknown | ApprovalRespondError::Cancelled => {
                RpcError::invalid(error.to_string())
            }
            ApprovalRespondError::Closed => RpcError::closed(),
        })
}

/// Replays the pending requests of every branch under each branch's
/// Approval lock; see `Approval::replay` for the ordering it guarantees.
pub(crate) fn replay(session: &Session, mut notify: impl FnMut(&str, &ApprovalRequestData)) {
    for branch in session.branches() {
        if let Ok(approval) = session.service::<Approval>(&branch.id) {
            approval.replay(|request| notify(&branch.id, request));
        }
    }
}
