use std::time::SystemTime;

use chrono::{DateTime, Utc};
use dim_agent_types::{BranchCreateFailedData, ModelId, ModelOutputOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::wire::{Event, EventContent};

pub const HINT: &str = "Run /report to report this error.";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    SessionRead,
    SessionFork,
    BranchCreate,
    ModelGeneration,
}

#[derive(Clone)]
pub struct Captured {
    pub session: String,
    pub branch: String,
    pub message: String,
    sequence: Option<u64>,
    pub operation: Operation,
    code: Option<String>,
    observed_at: DateTime<Utc>,
}

impl Captured {
    pub fn observed(
        session: &str,
        branch: &str,
        operation: Operation,
        code: Option<&str>,
        message: String,
    ) -> Self {
        Self {
            session: session.to_owned(),
            branch: branch.to_owned(),
            message,
            sequence: None,
            operation,
            code: code.map(str::to_owned),
            observed_at: SystemTime::now().into(),
        }
    }

    pub fn event(session: &str, branch: &str, event: &Event) -> Option<Self> {
        let (operation, error) = match &event.content {
            EventContent::ModelEnd(end) => match &end.outcome {
                ModelOutputOutcome::Failed { error, .. } => (Operation::ModelGeneration, error),
                _ => return None,
            },
            EventContent::BranchCreateFailed(BranchCreateFailedData::Failed { error, .. }) => {
                (Operation::BranchCreate, error)
            }
            _ => return None,
        };
        let mut capture = Self::observed(session, branch, operation, None, error.clone());
        capture.sequence = Some(event.sequence);
        Some(capture)
    }

    pub fn params(&self, session: bool) -> Value {
        let observed = json!({
            "operation": self.operation,
            "code": self.code,
            "observedAt": self.observed_at,
            "clientVersion": crate::version::VERSION,
        });
        let error = match self.sequence.filter(|_| session) {
            Some(sequence) => {
                json!({"kind":"timeline","branchId":self.branch,"sequence":sequence,"observed":observed})
            }
            None => json!({"kind":"observed","observed":observed}),
        };
        json!({"error":error})
    }
}

pub struct Request {
    pub panel: u64,
    pub action: Action,
}

pub enum Action {
    Prepare(Captured),
    Plan {
        draft: Draft,
        redact: bool,
        instructions: String,
    },
    Generate(Draft),
    Cancel {
        draft: Draft,
        generation: bool,
    },
}

#[derive(Clone)]
pub struct Draft {
    pub session: Option<String>,
    pub report_id: String,
    pub plan_id: String,
}

pub enum Reply {
    Prepared(Option<String>, Prepared),
    Plan(Plan),
    Generated(GeneratedReport),
    Failed {
        message: String,
        draft_missing: bool,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub report_id: String,
    pub default_model: Option<ModelId>,
    pub plan: Plan,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(rename = "generation")]
    _generation: LocalGeneration,
    pub plan_id: String,
    pub evidence: Evidence,
    pub instructions_status: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LocalGeneration {
    Local,
}

#[derive(Deserialize)]
pub struct Evidence {
    pub timeline: Option<Value>,
    pub unavailable: Option<EvidenceUnavailable>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceUnavailable {
    NoReference,
    FailureUnavailable,
    ReadFailed,
    Omitted,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
pub struct GeneratedReport {
    pub title: String,
    pub archive: Archive,
    pub sharing: Sharing,
}

#[derive(Deserialize)]
pub struct Archive {
    pub base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sharing {
    pub github_issue_url: String,
    pub discord_url: String,
    pub description: String,
}

pub fn session_admission_failed(error: &crate::Error) -> bool {
    matches!(
        dim_client::app_error(error),
        Some("session_closed" | "session_build_failed" | "session_protocol_version")
    ) || error
        .downcast_ref::<dim_client::RpcError>()
        .is_some_and(|rpc| rpc.code == -32603 && rpc.data.is_none())
}
