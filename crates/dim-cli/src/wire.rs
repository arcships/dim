//! Shared public RPC product-event boundary.
pub use dim_client::wire::*;

use dim_agent_types::ToolResult;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub session_id: String,
    pub model: dim_agent_types::ModelId,
    /// The managed worktree this creation made, when the request asked for
    /// one (#1451); a daemon without `session.create.worktree` sends none.
    #[serde(default)]
    pub worktree: Option<CreatedWorktree>,
}

/// The managed worktree a creation made: the task id and its directory
/// under the product home, from the `worktree` member of the reply.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub task_id: String,
    pub root: String,
}

#[derive(Deserialize)]
pub struct ProviderCatalogSnapshot {
    pub catalog: Option<ProviderCatalog>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalog {
    pub version: u32,
    pub models: Vec<CatalogModel>,
    pub disabled_model_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub capabilities: dim_agent_types::CatalogCapabilities,
    #[serde(default)]
    pub missing_limits: Vec<String>,
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub zdr: bool,
    pub metadata: Option<CatalogPreferences>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPreferences {
    pub reasoning: Option<dim_agent_types::ModelReasoning>,
    pub service_tier: Option<dim_agent_types::ModelServiceTier>,
    /// How the catalog identified this entry's id.
    #[serde(default)]
    pub identification: Option<Identification>,
    /// The row the projected entry is copied from.
    #[serde(default)]
    pub matched_provider_id: Option<String>,
    #[serde(default)]
    pub matched_model_id: Option<String>,
}

/// What identification made of one id, as the catalog and the management view
/// carry it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identification {
    /// The step's own word: `provider-exact`, `qualified`, `token`,
    /// `canonical`, `custom-provider`, `merge` or `ambiguous`.
    pub rule: String,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// One entry per limit the row did not declare, naming what supplied it.
    #[serde(default)]
    pub filled: Vec<Filled>,
}

/// One limit the row did not declare, and the entry that supplied it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filled {
    pub field: String,
    pub value: u64,
    pub provider_id: String,
    pub model_id: String,
}

/// One row an ambiguity refused.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub provider_id: String,
    pub model_id: String,
}

/// A Workflow run as `workflow.status` and `workflow.stop` return it: the
/// fields the task view reads. The run owner validates the whole record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub name: String,
    pub source: String,
    #[serde(deserialize_with = "timestamp")]
    pub created_at: u64,
    #[serde(default, deserialize_with = "optional_timestamp")]
    pub ended_at: Option<u64>,
    pub state: WorkflowState,
    pub stages: Vec<WorkflowStage>,
    pub log: Vec<WorkflowLog>,
    pub report: WorkflowReport,
    #[serde(default)]
    report_to: Option<WorkflowAddress>,
    started_by: WorkflowStarter,
}

impl WorkflowRun {
    /// The branch of `session_id` that started the run or receives its
    /// report; `None` when the run belongs to no branch of that Session.
    pub fn branch_of(&self, session_id: &str) -> Option<&str> {
        match &self.started_by {
            WorkflowStarter::Tool(starter) if starter.session_id == session_id => {
                Some(&starter.branch_id)
            }
            _ => self
                .report_to
                .as_ref()
                .filter(|to| to.session_id == session_id)
                .map(|to| to.branch_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowState {
    Running,
    Stopping,
    Completed,
    Failed,
    Stopped,
    Interrupted,
}

impl WorkflowState {
    pub fn unfinished(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkflowStage {
    pub id: String,
    pub step: u32,
    pub state: StageState,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub result_text: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageState {
    Waiting,
    Launching,
    Running,
    Completed,
    Failed,
    Stopped,
    Skipped,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkflowLog {
    #[serde(deserialize_with = "timestamp")]
    pub at: u64,
    pub message: String,
}

/// Report settlement, apart from the run's own outcome.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum WorkflowReport {
    None {},
    Pending {},
    Delivered {},
    Undeliverable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct WorkflowAddress {
    session_id: String,
    branch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum WorkflowStarter {
    Tool(WorkflowAddress),
    Management(Management),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Management {
    Management,
}

fn timestamp<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = String::deserialize(deserializer)?;
    crate::usage::parse_rfc3339_ms(&text)
        .ok_or_else(|| serde::de::Error::custom("Invalid Workflow timestamp"))
}

fn optional_timestamp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    timestamp(deserializer).map(Some)
}

/// The run a successful `workflow` tool result names and the state it saw:
/// `start` returns `{run_id, status}`, `status` and `stop` the stored `{run}`.
pub fn workflow_tool_run(result: &ToolResult) -> Option<(String, WorkflowState)> {
    #[derive(Deserialize)]
    struct Observed {
        run_id: String,
        state: WorkflowState,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Reply {
        Start {
            run_id: String,
            status: WorkflowState,
        },
        Record {
            run: Observed,
        },
    }
    if result.is_error == Some(true) {
        return None;
    }
    let structured = serde_json::Value::Object(result.structured_content.clone()?);
    Some(match serde_json::from_value(structured).ok()? {
        Reply::Start { run_id, status } => (run_id, status),
        Reply::Record { run } => (run.run_id, run.state),
    })
}

/// The run and outcome of a `<dim-workflow kind="report" ...>` input block.
pub fn workflow_report(text: &str) -> Option<(String, WorkflowState)> {
    let rest = text.strip_prefix(r#"<dim-workflow kind="report" run=""#)?;
    let (run, rest) = rest.split_once('"')?;
    let (status, _) = rest.strip_prefix(r#" status=""#)?.split_once('"')?;
    let status = serde_json::from_value(serde_json::Value::String(status.to_owned())).ok()?;
    Some((unescape(run), status))
}

/// The warnings a reply carries: the submit checks' work that did not happen
/// (Hooks A.6.2). An absent list is no warning.
pub fn warnings(reply: &Value) -> Vec<dim_agent_types::SubmitWarning> {
    reply
        .get("warnings")
        .and_then(|warnings| serde_json::from_value(warnings.clone()).ok())
        .unwrap_or_default()
}

/// Reverses the marker's attribute escaping.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
