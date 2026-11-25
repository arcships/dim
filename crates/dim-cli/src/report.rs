use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_types::{BranchCreateFailedData, ModelId, ModelOutputOutcome};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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
    observed_at: Timestamp,
}

impl Captured {
    pub fn observed(operation: Operation, code: Option<&str>, message: String) -> Self {
        Self {
            message,
            operation,
            code: code.map(str::to_owned),
            observed_at: Timestamp::now(),
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
            "observedAt": dim_agent_capabilities::timestamp::format(self.observed_at),
            "clientVersion": dim_agent_capabilities::build::current(),
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
        include_conversation: bool,
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
    pub files: Option<ReportFiles>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportFiles {
    #[serde(rename = "report.md")]
    pub report: String,
    #[serde(rename = "diagnostics.json")]
    pub diagnostics: String,
}

impl ReportFiles {
    pub fn preview(&self) -> String {
        let mut text = String::new();
        for (name, content) in [
            ("report.md", &self.report),
            ("diagnostics.json", &self.diagnostics),
        ] {
            text.push_str(&format!("--- {name} ---\n"));
            for ch in content.chars() {
                if ch != '\n'
                    && (ch.is_control()
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
                {
                    text.extend(ch.escape_debug());
                } else {
                    text.push(ch);
                }
            }
            text.push_str("\n\n");
        }
        text
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LocalGeneration {
    Local,
}

#[derive(Deserialize)]
pub struct Evidence {
    #[serde(default)]
    pub conversation: Vec<serde::de::IgnoredAny>,
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
    pub files: Option<ReportFiles>,
    pub archive: Archive,
    pub sharing: Sharing,
}

impl GeneratedReport {
    pub fn zip_bytes(&self) -> crate::Result<Vec<u8>> {
        const LIMIT: usize = 64 * 1024;
        if self.archive.base64.len() > LIMIT.div_ceil(3) * 4 {
            return Err(crate::ui::i18n::t("Invalid report ZIP").into());
        }
        let bytes = STANDARD.decode(&self.archive.base64)?;
        if bytes.len() > LIMIT || !bytes.starts_with(b"PK\x03\x04") {
            return Err(crate::ui::i18n::t("Invalid report ZIP").into());
        }
        Ok(bytes)
    }
}

pub const EMAIL: &str = "feedback@arcships.ai";
pub const EMAIL_SUBJECT: &str = "Dim report";

#[derive(Clone, Copy, Default)]
pub enum MailScope {
    #[default]
    ReportOnly,
    InternalRegression,
}

impl MailScope {
    fn purpose(self) -> &'static str {
        match self {
            Self::ReportOnly => "Issue investigation only; no evaluation authorization.",
            Self::InternalRegression => {
                "Issue investigation and team-internal regression evaluation of this ZIP only. No external model providers, training, or publication."
            }
        }
    }

    pub fn body(self, bytes: &[u8]) -> String {
        format!(
            "Purpose: {}\r\nSHA-256: {:x}",
            self.purpose(),
            Sha256::digest(bytes)
        )
    }
}

pub fn mailto(body: &str) -> String {
    fn encode(text: &str) -> String {
        use std::fmt::Write as _;
        let mut encoded = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                encoded.push(char::from(byte));
            } else {
                write!(encoded, "%{byte:02X}").expect("writing to a string");
            }
        }
        encoded
    }
    format!(
        "mailto:{EMAIL}?subject={}&body={}",
        encode(EMAIL_SUBJECT),
        encode(body)
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_observed_instant_keeps_the_stored_fraction_bytes() {
        let captured = Captured {
            message: "failed".into(),
            operation: Operation::SessionRead,
            code: None,
            observed_at: "2030-01-01T00:00:00.100Z".parse().unwrap(),
        };
        assert_eq!(
            captured.params()["error"]["observed"]["observedAt"],
            json!("2030-01-01T00:00:00.100Z")
        );
    }
}
