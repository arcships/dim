use std::time::SystemTime;

use chrono::{DateTime, Utc};
use dim_agent_types::{BranchCreateFailedData, ModelId, ModelOutputOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::wire::{Event, EventContent};

pub const HINT: &str = "Run /report to create a report.";

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
    pub message: String,
    pub operation: Operation,
    code: Option<String>,
    observed_at: DateTime<Utc>,
}

impl Captured {
    pub fn observed(operation: Operation, code: Option<&str>, message: String) -> Self {
        Self {
            message,
            operation,
            code: code.map(str::to_owned),
            observed_at: SystemTime::now().into(),
        }
    }

    pub fn event(event: &Event) -> Option<Self> {
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
        Some(Self::observed(operation, None, error.clone()))
    }

    pub fn params(&self) -> Value {
        let observed = json!({
            "operation": self.operation,
            "code": self.code,
            "observedAt": self.observed_at,
            "clientVersion": crate::version::VERSION,
        });
        json!({"error":{"kind":"observed","observed":observed}})
    }
}

#[derive(Clone)]
pub enum Target {
    Session(String),
    Error(Captured),
}

pub struct Request {
    pub panel: u64,
    pub action: Action,
}

pub enum Action {
    Prepare(Target),
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
    pub timeline: Option<TimelineEvidence>,
    pub unavailable: Option<EvidenceUnavailable>,
}

#[derive(Deserialize)]
pub struct TimelineEvidence {
    pub recent: Vec<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceUnavailable {
    NoReference,
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
