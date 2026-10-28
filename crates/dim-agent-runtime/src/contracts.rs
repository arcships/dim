use crate::{Branch, Result};
use dim_agent_types::{ModelSelection, PromptScope};
use rutis::host::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;

/// Inputs admitted and not yet consumed by Loop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputSnapshot {
    pub queued: usize,
}
/// Output of the running model request not yet written to the Timeline.
/// It is display state of one invocation, never rebuilt from events (agent-029).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProgress {
    pub turn_id: Option<String>,
    pub request_id: Option<String>,
    /// Text of this request not yet published as `step/text`: committed
    /// and unpublished, then received and not yet committed.
    pub text: String,
    /// Unicode scalar values of this request's `step/text` deltas, in
    /// sequence order, that precede `text`.
    pub text_offset: u64,
    /// Thinking of this request not yet published as `step/thinking`:
    /// committed blocks, then the block in progress.
    pub thinking: String,
    /// Unicode scalar values of this request's `step/thinking` deltas, in
    /// sequence order, that precede `thinking`.
    pub thinking_offset: u64,
    /// Started calls whose arguments are not complete, in start order.
    pub tools: Vec<ToolProgress>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolProgress {
    pub call_id: String,
    pub name: String,
    /// Raw argument text received so far.
    pub arguments: String,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoopPhase {
    /// No unclosed turn.
    #[default]
    Idle,
    /// An unclosed turn.
    Running,
    /// An unclosed turn that has been interrupted.
    Stopping,
}
/// Execution state of one branch, a pure function of Loop's dispatched events.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopSnapshot {
    pub phase: LoopPhase,
    pub turn_id: Option<String>,
}

pub trait LoopState: Send + Sync + 'static {
    fn state(&self) -> LoopSnapshot;
    fn watch(&self) -> watch::Receiver<LoopSnapshot>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSourceTarget {
    pub source: String,
    pub scope: PromptScope,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigureUpdate {
    pub sources: Vec<PromptSourceTarget>,
    pub model: Option<ModelSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureOutcome {
    Unchanged,
    Adopted,
    Superseded,
}

pub trait ConfigureControl: Send + Sync + 'static {
    fn configure(
        self: Arc<Self>,
        update: ConfigureUpdate,
    ) -> BoxFuture<'static, Result<ConfigureOutcome>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRecord {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_point: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_anchor: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_point: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_anchor: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<dim_agent_types::BranchSource>,
}

impl From<BranchRecord> for TimelineInfo {
    fn from(record: BranchRecord) -> Self {
        Self {
            id: record.id,
            parent: record.parent,
            branch_point: record.branch_point,
            create_anchor: record.create_anchor,
            source: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchOptions {
    pub prompt: String,
    pub source: dim_agent_types::BranchSource,
}
pub trait TimelineTreeControl: Send + Sync + 'static {
    fn begin_close(&self);
    fn restore(&self, branches: Vec<crate::StoredBranch>) -> BoxFuture<'_, Result<()>>;
    fn create_main(&self) -> BoxFuture<'_, Result<Branch>>;
    fn branch(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'_, Result<Branch>>;
    fn get(&self, id: &str) -> Option<Branch>;
    /// Ends the line `id` with `source`; only the target is checked.
    fn end(&self, id: String, source: dim_agent_types::BranchSource) -> BoxFuture<'_, Result<()>>;
    /// The fixed head of an ended line, `None` for an open or unknown one.
    fn ended_head(&self, id: &str) -> Option<u64>;
    fn list(&self) -> Vec<TimelineInfo>;
    fn close(&self) -> BoxFuture<'_, Result<()>>;
}
