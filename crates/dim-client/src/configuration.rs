//! One line's adopted configuration, read through `session.model.get`.
use crate::{
    Client, Result,
    wire::{AdoptedConfiguration, Configuration},
};
use serde_json::json;

/// The configuration `branch` adopted at `through`, or at its head when
/// omitted, with that line's own committed cutoff. The cutoff is the
/// watermark a client follows from: only that line's later
/// `context/configured` events apply to it (design §14).
pub async fn adopted(
    client: &mut Client,
    session_id: &str,
    branch: &str,
    through: Option<u64>,
) -> Result<(u64, Option<Configuration>)> {
    let mut params = json!({"sessionId": session_id, "branchId": branch});
    if let Some(through) = through {
        params["through"] = json!(through);
    }
    let reply: AdoptedConfiguration = client.typed("session.model.get", params).await?;
    if reply.branch_id != branch {
        return Err("session.model.get answered another line".into());
    }
    Ok((reply.through, reply.configuration.map(Configuration::from)))
}
