use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingExec {
    pub event_id: String,
    #[serde(with = "crate::safe_integer")]
    pub sequence: u64,
    #[serde(with = "crate::safe_integer")]
    pub started_at: u64,
    pub task_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The call's index in a multi-call model response. Absent for a
    /// single-call response or when the response is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingDelegation {
    pub event_id: String,
    #[serde(with = "crate::safe_integer")]
    pub sequence: u64,
    #[serde(with = "crate::safe_integer")]
    pub started_at: u64,
    pub request: crate::CollaborationRequestData,
    pub delivery: Option<crate::CollaborationDelivery>,
    /// The call's index in a multi-call model response. Absent for a
    /// single-call response or when the response is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// A background command that ended: its start, as [`PendingExec`] carries
/// it, and the `exec/ended` fact with that fact's time.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinishedExec {
    pub start: PendingExec,
    #[serde(with = "crate::safe_integer")]
    pub ended_at: u64,
    pub end: crate::ExecEndedData,
}

/// A delegation that settled: its request and latest delivery, as
/// [`PendingDelegation`] carries them, and the terminal fact with its time:
/// the `collaboration/result`, or no result when the delivery failed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinishedDelegation {
    pub start: PendingDelegation,
    #[serde(with = "crate::safe_integer")]
    pub ended_at: u64,
    pub result: Option<crate::CollaborationResultData>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveOutput {
    pub request_id: String,
    #[serde(with = "crate::safe_integer")]
    pub text_characters: u64,
    #[serde(with = "crate::safe_integer")]
    pub thinking_characters: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRead {
    pub branch_id: String,
    #[serde(with = "crate::safe_integer")]
    pub through: u64,
    pub call: Option<crate::ToolUseData>,
}
