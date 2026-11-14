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
