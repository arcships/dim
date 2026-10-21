//! Frozen version 1 event contract and borrowing output adapter.
use crate::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use crate::TurnEvent;

/// The flat, summary-oriented diagnostics of the frozen contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionDiagnosticsRecord {
    pub reason: CompactionReason,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub trigger: Option<CompactionTrigger>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub source_run_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub baseline_source: Option<BaselineSource>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub coverage: Option<CompactionCoverage>,
    #[serde(with = "crate::finite_number")]
    pub candidate_message_count: f64,
    #[serde(with = "crate::finite_number")]
    pub input_message_count: f64,
    #[serde(with = "crate::finite_number")]
    pub retained_message_count: f64,
    #[serde(with = "crate::finite_number")]
    pub retained_token_budget: f64,
    #[serde(with = "crate::finite_number")]
    pub retained_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub serialized_chars: f64,
    #[serde(with = "crate::finite_number")]
    pub estimated_input_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub estimated_compressed_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub estimated_post_compaction_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub summary_input_budget: f64,
    #[serde(with = "crate::finite_number")]
    pub preflight_input_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub preflight_output_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub compacted_tool_payload_count: f64,
    #[serde(with = "crate::finite_number")]
    pub dropped_message_count: f64,
    #[serde(with = "crate::finite_number")]
    pub chunk_count: f64,
    #[serde(with = "crate::finite_number")]
    pub summary_chars: f64,
    #[serde(with = "crate::finite_number")]
    pub estimated_summary_tokens: f64,
    #[serde(with = "crate::finite_number")]
    pub model_call_count: f64,
    #[serde(with = "crate::finite_number")]
    pub duration_ms: f64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub source_turn_id: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub baseline_prompt_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub delta_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub planned_output_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub min_visible_output_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub requested_thinking_budget_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub adjusted_thinking_budget_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub reasoning_effort_budget_estimate_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub effective_planned_output_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub system_prompt_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub chunk_target_input_budget: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub max_chunk_input_tokens: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub summary_request_context_ratio: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub request_envelope_margin: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub recursive_depth: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub omitted_token_estimate: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub thinking_budget_clamped: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub budget_estimate_source: Option<BudgetEstimateSource>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub compaction_reasoning_effort: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub tail_truncated_message_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub usage: Option<CompactionUsage>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub failure_code: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub fallback_reason: Option<CompactionFallbackReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMessageData {
    pub text: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mode: Option<InputMode>,
}
impl InputMessageData {
    pub fn effective_mode(&self) -> InputMode {
        self.mode.unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum InputEvent {
    #[serde(rename = "message")]
    InputMessage(InputMessageData),
    #[serde(rename = "applied")]
    InputApplied(crate::InputAppliedData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPrepareData {
    pub turn_id: String,
    pub request_id: String,
    #[serde(with = "crate::safe_integer")]
    pub through: u64,
    pub request: Arc<ModelRequest>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub recovery: Option<ProviderOutputBudgetFacts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextCompactionResult {
    Completed {
        cursor: String,
        summary: String,
        diagnostics: Box<CompactionDiagnosticsRecord>,
    },
    Skipped {
        reason: String,
        diagnostics: Box<CompactionDiagnosticsRecord>,
    },
    Failed {
        error: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        diagnostics: Option<Box<CompactionDiagnosticsRecord>>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionData {
    pub turn_id: String,
    pub request_id: String,
    #[serde(with = "crate::safe_integer")]
    pub through: u64,
    #[serde(flatten)]
    pub outcome: ContextCompactionResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum ContextEvent {
    #[serde(rename = "prepare")]
    Prepare(ContextPrepareData),
    #[serde(rename = "compaction")]
    Compaction(ContextCompactionData),
    #[serde(rename = "budget")]
    Budget(crate::ContextBudgetData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCallData {
    pub turn_id: String,
    pub request_id: String,
    pub request: Arc<ModelRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum ModelEvent {
    #[serde(rename = "call")]
    ModelCall(ModelCallData),
    #[serde(rename = "usage")]
    ModelUsage(crate::ModelUsageData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResult {
    pub content: ToolContent,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub structured_content: Option<JsonObject>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub is_error: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub blob_refs: Option<Vec<BlobRef>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub background_task: Option<BackgroundTaskInfo>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub context_messages: Option<Vec<Message>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallResult {
    pub call_id: String,
    pub result: ToolResult,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCompletedData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub result: ToolResult,
    pub call_event_id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultData {
    pub turn_id: String,
    pub request_id: String,
    pub call_event_id: String,
    pub results: Vec<ToolCallResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum StepEvent {
    #[serde(rename = "model-start")]
    ModelStart(crate::ModelStartData),
    #[serde(rename = "thinking")]
    Thinking(crate::ThinkingData),
    #[serde(rename = "text")]
    Text(crate::TextData),
    #[serde(rename = "tool-use")]
    ToolUse(crate::ToolUseData),
    #[serde(rename = "model-output-end")]
    ModelOutputEnd(crate::ModelOutputEndData),
    #[serde(rename = "tool-call")]
    ToolCall(crate::ToolCallData),
    #[serde(rename = "tool-start")]
    ToolStart(crate::ToolStartData),
    #[serde(rename = "tool-completed")]
    ToolCompleted(ToolCompletedData),
    #[serde(rename = "tool-result")]
    ToolResult(ToolResultData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchCreateData {
    pub turn_id: String,
    pub request_id: String,
    pub branch: String,
    pub prompt: String,
    pub call_id: String,
}

/// Background task facts of the frozen TypeScript union; Rust never produces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStartedData {
    pub turn_id: String,
    pub request_id: String,
    pub task_id: String,
    pub call_id: String,
    pub tool: String,
    pub wake: WakePolicy,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub description: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(rename = "outputPath")]
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum TaskEvent {
    #[serde(rename = "started")]
    Started(TaskStartedData),
    #[serde(rename = "ended")]
    Ended(TaskEndedData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEndedData {
    pub turn_id: String,
    pub request_id: String,
    pub task_id: String,
    pub status: TaskStatus,
    pub result: ToolResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum BranchEvent {
    #[serde(rename = "create")]
    Create(BranchCreateData),
    #[serde(rename = "start")]
    Start(BranchStartData),
}

/// Version 1 knows only child branches, which have a parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchStartData {
    pub parent: String,
    #[serde(with = "crate::safe_integer")]
    pub create_event: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EventKind {
    Input(InputEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    Model(ModelEvent),
    Step(StepEvent),
    Task(TaskEvent),
    Branch(BranchEvent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub id: String,
    #[serde(with = "crate::event_sequence")]
    pub sequence: u64,
    #[serde(with = "crate::safe_integer")]
    pub create_at: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub tags: Option<Vec<String>>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Only the outbound boundary constructs this view; internal controls have no v1 view.
#[derive(Debug, Clone, Copy)]
pub struct EventRef<'a>(&'a crate::AgentEvent);
impl<'a> EventRef<'a> {
    pub fn new(event: &'a crate::AgentEvent) -> Option<Self> {
        let supported = !event.kind.is_control()
            && !matches!(
                event.kind,
                crate::EventKind::InputConsumed(_)
                    | crate::EventKind::ToolUseStart(_)
                    | crate::EventKind::ModelContent(_)
                    | crate::EventKind::PromptChanged(_)
                    | crate::EventKind::ContextConfigured(_)
                    | crate::EventKind::ContextScope(_)
                    | crate::EventKind::BranchCreateFailed(_)
                    | crate::EventKind::BranchResume(_)
                    | crate::EventKind::SessionForked(_)
                    | crate::EventKind::CollaborationRequest(_)
                    | crate::EventKind::CollaborationCancel(_)
                    | crate::EventKind::CollaborationClose(_)
                    | crate::EventKind::CollaborationMessage(_)
                    | crate::EventKind::CollaborationDelivered(_)
                    | crate::EventKind::CollaborationResult(_)
                    | crate::EventKind::PermissionChanged(_)
                    | crate::EventKind::ExecStarted(_)
                    | crate::EventKind::ExecEnded(_)
                    // The frozen compaction form cannot carry the changed
                    // record; the command and the start are new.
                    | crate::EventKind::ContextCompact(_)
                    | crate::EventKind::ContextCompactionStart(_)
                    | crate::EventKind::ContextCompaction(_)
                    // Extension events reach only a dynamic protocol (ADR 001).
                    | crate::EventKind::Extension(_)
            )
            && match &event.kind {
                crate::EventKind::BranchCreate(data) => {
                    matches!(data.source, crate::BranchSource::Tool { .. })
                }
                crate::EventKind::BranchStart(data) => data.parent.is_some(),
                // Results closing undispatched calls have no frozen form (ADR agent-029).
                crate::EventKind::ToolResult(data) => data.call_event_id.is_some(),
                _ => true,
            };
        supported.then_some(Self(event))
    }
}
impl Serialize for EventRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Envelope<'a> {
            id: &'a str,
            #[serde(with = "crate::event_sequence")]
            sequence: u64,
            #[serde(with = "crate::safe_integer")]
            create_at: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            tags: Option<&'a Vec<String>>,
            #[serde(flatten)]
            kind: KindRef<'a>,
        }
        Envelope {
            id: &self.0.id,
            sequence: self.0.sequence,
            create_at: self.0.create_at,
            tags: self.0.tags.as_ref(),
            kind: KindRef(&self.0.kind),
        }
        .serialize(serializer)
    }
}
struct KindRef<'a>(&'a crate::EventKind);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultRef<'a> {
    content: &'a crate::ToolContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<&'a crate::JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_refs: Option<&'a Vec<crate::BlobRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background_task: Option<&'a crate::BackgroundTaskInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_context_messages")]
    context_messages: Option<&'a Vec<crate::Message>>,
}
impl<'a> From<&'a crate::ToolResult> for ToolResultRef<'a> {
    fn from(value: &'a crate::ToolResult) -> Self {
        Self {
            content: &value.content,
            structured_content: value.structured_content.as_ref(),
            is_error: value.is_error,
            blob_refs: value.blob_refs.as_ref(),
            background_task: value.background_task.as_ref(),
            context_messages: value.context_messages.as_ref(),
        }
    }
}
impl Serialize for KindRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Tagged<T: Serialize> {
            #[serde(rename = "type")]
            event_type: &'static str,
            name: &'static str,
            data: T,
        }
        #[derive(Serialize)]
        struct Input<'a> {
            text: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            mode: Option<crate::InputMode>,
        }
        #[derive(Serialize)]
        struct Prepare<'a> {
            turn_id: &'a str,
            request_id: &'a str,
            #[serde(with = "crate::safe_integer")]
            through: u64,
            #[serde(serialize_with = "serialize_request")]
            request: &'a Arc<crate::ModelRequest>,
            #[serde(skip_serializing_if = "Option::is_none")]
            recovery: Option<&'a crate::ProviderOutputBudgetFacts>,
        }
        #[derive(Serialize)]
        struct Call<'a> {
            turn_id: &'a str,
            request_id: &'a str,
            #[serde(serialize_with = "serialize_request")]
            request: &'a Arc<crate::ModelRequest>,
        }
        #[derive(Serialize)]
        struct Completed<'a> {
            turn_id: &'a str,
            request_id: &'a str,
            call_id: &'a str,
            result: ToolResultRef<'a>,
            call_event_id: &'a str,
        }
        #[derive(Serialize)]
        struct ResultItem<'a> {
            call_id: &'a str,
            result: ToolResultRef<'a>,
        }
        #[derive(Serialize)]
        struct Results<'a> {
            turn_id: &'a str,
            request_id: &'a str,
            call_event_id: &'a str,
            results: Vec<ResultItem<'a>>,
        }
        if let crate::EventKind::InputMessage(data) = self.0 {
            let text = data
                .user
                .iter()
                .flat_map(|user| &user.content)
                .filter_map(|block| match block {
                    crate::ContentBlock::Text { text } => Some(text.as_str()),
                    crate::ContentBlock::Image { .. } => None,
                })
                .collect::<String>();
            Tagged {
                event_type: "input",
                name: "message",
                data: Input {
                    text: &text,
                    mode: data.mode,
                },
            }
            .serialize(serializer)
        } else if let crate::EventKind::ContextPrepare(data) = self.0 {
            Tagged {
                event_type: "context",
                name: "prepare",
                data: Prepare {
                    turn_id: &data.turn_id,
                    request_id: &data.request_id,
                    through: data.through,
                    request: &data.request,
                    recovery: data.recovery.as_ref(),
                },
            }
            .serialize(serializer)
        } else if let crate::EventKind::ModelCall(data) = self.0 {
            Tagged {
                event_type: "model",
                name: "call",
                data: Call {
                    turn_id: &data.turn_id,
                    request_id: &data.request_id,
                    request: &data.request,
                },
            }
            .serialize(serializer)
        } else if let crate::EventKind::ToolCompleted(data) = self.0 {
            Tagged {
                event_type: "step",
                name: "tool-completed",
                data: Completed {
                    turn_id: &data.turn_id,
                    request_id: &data.request_id,
                    call_id: &data.call_id,
                    result: (&data.result).into(),
                    call_event_id: &data.call_event_id,
                },
            }
            .serialize(serializer)
        } else if let crate::EventKind::ToolResult(data) = self.0 {
            let Some(call_event_id) = &data.call_event_id else {
                unreachable!("undispatched step/tool-result has no v1 view")
            };
            Tagged {
                event_type: "step",
                name: "tool-result",
                data: Results {
                    turn_id: &data.turn_id,
                    request_id: &data.request_id,
                    call_event_id,
                    results: data
                        .results
                        .iter()
                        .map(|item| ResultItem {
                            call_id: &item.call_id,
                            result: (&item.result).into(),
                        })
                        .collect(),
                },
            }
            .serialize(serializer)
        } else if let crate::EventKind::BranchCreate(data) = self.0 {
            let crate::BranchSource::Tool {
                turn_id,
                request_id,
                call_id,
            } = &data.source
            else {
                unreachable!("host branch/create has no v1 view")
            };
            #[derive(Serialize)]
            struct Create<'a> {
                turn_id: &'a str,
                request_id: &'a str,
                branch: &'a str,
                prompt: &'a str,
                call_id: &'a str,
            }
            #[derive(Serialize)]
            struct Branch<'a> {
                #[serde(rename = "type")]
                event_type: &'static str,
                name: &'static str,
                data: Create<'a>,
            }
            Branch {
                event_type: "branch",
                name: "create",
                data: Create {
                    turn_id,
                    request_id,
                    branch: &data.branch,
                    prompt: &data.prompt,
                    call_id,
                },
            }
            .serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

/// Holds the existing shared event across a connection queue without cloning its body.
#[derive(Debug, Clone)]
pub struct SharedEvent(Arc<crate::AgentEvent>);
impl SharedEvent {
    pub fn new(event: Arc<crate::AgentEvent>) -> Option<Self> {
        EventRef::new(&event)?;
        Some(Self(event))
    }
}
impl Serialize for SharedEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        EventRef(&self.0).serialize(serializer)
    }
}

// Version 1 exposes display content only; native continuation stays on the Timeline.
fn strip_continuation(messages: &mut serde_json::Value) {
    for message in messages.as_array_mut().into_iter().flatten() {
        for part in message["content"].as_array_mut().into_iter().flatten() {
            if let Some(part) = part.as_object_mut() {
                part.remove("providerMetadata");
            }
        }
    }
}
fn serialize_request<S: serde::Serializer>(
    request: &Arc<crate::ModelRequest>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(request).map_err(serde::ser::Error::custom)?;
    value
        .as_object_mut()
        .expect("request is an object")
        .remove("capabilitySnapshot");
    strip_continuation(&mut value["messages"]);
    value.serialize(serializer)
}
fn serialize_context_messages<S: serde::Serializer>(
    messages: &Option<&Vec<crate::Message>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(messages).map_err(serde::ser::Error::custom)?;
    strip_continuation(&mut value);
    value.serialize(serializer)
}
