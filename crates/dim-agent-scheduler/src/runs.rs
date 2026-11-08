//! The run history file `scheduler/runs.json` (A.1, A.7): one inner v1 JSON
//! document inside the host's `{version, text}` envelope, which
//! [`PluginFileTransactions`] reads and writes.
use crate::document::{canonical_instant, format_instant};
use chrono::{DateTime, Utc};
use dim_agent_types::PermissionPolicy;
use dim_app_server::plugin_files::{PluginFileError, PluginFileTransactions};
use serde::{Deserialize, Serialize};

pub const RUNS_FILE: &str = "scheduler/runs.json";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct SchedulerError {
    pub code: &'static str,
    pub message: String,
}

impl SchedulerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid_data(message: impl Into<String>) -> Self {
        Self::new("invalid_data", message)
    }
}

impl From<PluginFileError> for SchedulerError {
    fn from(error: PluginFileError) -> Self {
        match error {
            PluginFileError::InvalidData => Self::invalid_data(error.to_string()),
            PluginFileError::Io(_) => Self::new("io", error.to_string()),
            PluginFileError::Locked => Self::new("locked", error.to_string()),
            PluginFileError::Closed | PluginFileError::InvalidPath => {
                Self::new("service_unavailable", error.to_string())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Skipped,
    Missed,
    Launching,
    Submitted,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Lost,
}

impl RunState {
    /// The wire name, as the file and `schedule.history` spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Missed => "missed",
            Self::Launching => "launching",
            Self::Submitted => "submitted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Lost => "lost",
        }
    }

    /// Terminal states carry `ended_at` (A.1).
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Launching | Self::Submitted | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrigin {
    Launch,
    Reuse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTo {
    pub session_id: String,
    pub branch_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub run_id: String,
    pub job_id: String,
    /// `manual` or a canonical instant.
    pub window: String,
    pub state: RunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<SessionOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_event_id: Option<String>,
    pub submission_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<ReportTo>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub detail: String,
}

impl Run {
    /// The record invariants of A.1 and A.7. A violation makes the whole file
    /// invalid, so the bytes stay unchanged.
    pub fn validate(&self) -> Result<(), String> {
        let timestamp = |text: &str| canonical_instant(text).is_some();
        if !timestamp(&self.created_at)
            || self
                .deadline_at
                .as_deref()
                .is_some_and(|text| !timestamp(text))
            || self
                .ended_at
                .as_deref()
                .is_some_and(|text| !timestamp(text))
            || (self.window != "manual" && !timestamp(&self.window))
        {
            return Err(format!("run {} has a non-canonical timestamp", self.run_id));
        }
        let session_fields = [
            self.session_id.is_some(),
            self.session_origin.is_some(),
            self.policy.is_some(),
        ]
        .iter()
        .filter(|present| **present)
        .count();
        let accepted = !matches!(self.state, RunState::Skipped | RunState::Missed);
        let has_input = self.input_event_id.is_some();
        let consistent = self.state.is_terminal() == self.ended_at.is_some()
            && (!accepted || self.deadline_at.is_some())
            && !(self.state == RunState::Missed && self.deadline_at.is_some())
            && (session_fields == 0 || session_fields == 3)
            && !(matches!(
                self.state,
                RunState::Submitted | RunState::Running | RunState::Completed | RunState::Cancelled
            ) && (!has_input || session_fields != 3))
            && !(self.state == RunState::Launching && has_input)
            && !(self.state == RunState::Missed
                && (session_fields != 0 || has_input || self.report_to.is_some()))
            && !(self.state == RunState::Skipped
                && (has_input
                    || (self.deadline_at.is_none()
                        && (session_fields != 0 || self.report_to.is_some()))))
            && !(has_input && session_fields != 3)
            && self.submission_id == format!("scheduler:{}", self.run_id);
        if consistent {
            Ok(())
        } else {
            Err(format!("run {} is inconsistent", self.run_id))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFile {
    version: u32,
    runs: Vec<Run>,
}

/// Decodes the inner text of the file; `None` is a missing file, which holds
/// no runs.
fn decode(text: Option<&str>) -> Result<Vec<Run>, SchedulerError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let file: RunFile = serde_json::from_str(text)
        .map_err(|error| SchedulerError::invalid_data(format!("runs.json: {error}")))?;
    if file.version != FORMAT_VERSION {
        return Err(SchedulerError::invalid_data(format!(
            "runs.json has unknown version {}",
            file.version
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for run in &file.runs {
        if !seen.insert(run.run_id.as_str()) {
            return Err(SchedulerError::invalid_data(format!(
                "runs.json repeats run {}",
                run.run_id
            )));
        }
        run.validate().map_err(SchedulerError::invalid_data)?;
    }
    Ok(file.runs)
}

fn encode(runs: Vec<Run>) -> String {
    serde_json::to_string(&RunFile {
        version: FORMAT_VERSION,
        runs,
    })
    .expect("run records contain only serializable fields")
}

/// Reads the history. A file that is not valid v1 is an error, not an empty
/// history.
pub async fn read(files: &PluginFileTransactions) -> Result<Vec<Run>, SchedulerError> {
    files
        .transact(RUNS_FILE, |current| (None, decode(current)))
        .await?
}

/// Applies `change` to the history under the file lock. `change` returns
/// whether it modified the runs; an unmodified history is not written back.
pub async fn update<R: Send + 'static>(
    files: &PluginFileTransactions,
    change: impl FnOnce(&mut Vec<Run>) -> Result<(bool, R), SchedulerError> + Send + 'static,
) -> Result<R, SchedulerError> {
    files
        .transact(RUNS_FILE, move |current| {
            let mut runs = match decode(current) {
                Ok(runs) => runs,
                Err(error) => return (None, Err(error)),
            };
            match change(&mut runs) {
                Ok((true, value)) => (Some(encode(runs)), Ok(value)),
                Ok((false, value)) => (None, Ok(value)),
                Err(error) => (None, Err(error)),
            }
        })
        .await?
}

/// The timestamp field, rendered as the file stores it.
pub fn stamp(instant: DateTime<Utc>) -> String {
    format_instant(instant)
}
