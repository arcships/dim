use crate::*;
use serde::{Deserialize, Serialize, de};

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InputMode {
    #[serde(rename = "queue")]
    #[default]
    Queue,
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "append")]
    Append,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterruptScope {
    Turn,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopInterruptData {
    pub reason: String,
    pub scope: InterruptScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCallData {
    pub name: String,
    pub input: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequestData {
    pub approval_id: String,
    pub source: BranchSource,
    pub call: ApprovalCallData,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRespondData {
    pub approval_id: String,
    pub decision: ApprovalDecision,
}

/// Ends a committed request that no accepted decision settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCancelledData {
    pub approval_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    #[serde(rename = "end_turn")]
    EndTurn,
    #[serde(rename = "tool_use")]
    ToolUse,
    #[serde(rename = "stop_sequence")]
    StopSequence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInput {
    pub content: Vec<ContentBlock>,
}

/// Runtime content a capability places in the model context. The renderer
/// puts `text` verbatim as one user message where the input is consumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InputBlock {
    Raw {
        text: String,
        /// Content that stays in effect: a later block with the same key
        /// supersedes this one, and compaction replays the latest of each
        /// live key.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        key: Option<String>,
        /// Ends `key`: this block is delivered, then the key is not replayed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        retire: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecStartedData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub task_id: String,
    pub description: String,
    pub output_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecEndedData {
    pub task_id: String,
    pub status: ExecStatus,
    pub result: ToolResult,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMessageData {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<InputBlock>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mode: Option<InputMode>,
}

impl InputMessageData {
    pub fn text(text: impl Into<String>, mode: Option<InputMode>) -> Self {
        let text = text.into();
        Self {
            user: (!text.is_empty()).then(|| UserInput {
                content: vec![ContentBlock::Text { text }],
            }),
            mode,
            ..Self::default()
        }
    }

    pub fn effective_mode(&self) -> InputMode {
        self.mode.unwrap_or(if self.user.is_none() {
            InputMode::Append
        } else {
            InputMode::Queue
        })
    }

    /// Checks input arriving from outside: user content, if present, is not
    /// all empty text, and the input carries user content or blocks.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.user.as_ref().is_some_and(|user| {
            user.content
                .iter()
                .all(|block| matches!(block, ContentBlock::Text { text } if text.is_empty()))
        }) {
            return Err("user input content must not be empty");
        }
        if self.user.is_none() && self.blocks.is_empty() {
            return Err("input must contain user content or blocks");
        }
        Ok(())
    }
}

/// A command to withdraw an input InputQueue still holds unapplied. It is
/// never a fact about the input by itself: only `input/deleted` is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputWithdrawData {
    pub input_id: String,
    /// Text for clients to show with the deleted input; no decision reads it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub reason: Option<String>,
}
impl InputWithdrawData {
    pub const MAX_REASON_BYTES: usize = 2048;
    /// Checks a command arriving from outside.
    pub fn validate_external(&self) -> Result<(), &'static str> {
        if self.input_id.is_empty() {
            return Err("input_id must not be empty");
        }
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > Self::MAX_REASON_BYTES)
        {
            return Err("reason exceeds 2 KiB");
        }
        Ok(())
    }
}

/// InputQueue's result for a withdrawn input: it is never applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputDeletedData {
    pub input_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAppliedData {
    pub input_id: String,
    /// The input's effective mode, which tells Loop how to consume it.
    /// A Rust extension of the frozen TypeScript contract; absent only in
    /// samples written against that contract.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mode: Option<InputMode>,
}

/// Loop took an applied input into the turn; history places the input here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConsumedData {
    pub input_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnStartData {
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepData {
    pub turn_id: String,
    pub request_id: String,
}

pub type ModelStartData = StepData;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaData {
    pub turn_id: String,
    pub request_id: String,
    pub delta: String,
}

pub type ThinkingData = DeltaData;
pub type TextData = DeltaData;

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
    pub configuration: Option<PromptReference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub prompt: Option<PromptReference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub recovery: Option<ProviderOutputBudgetFacts>,
}

/// A client asks the Loop to compact its branch; the Loop serves it as a
/// compaction step at its next step boundary, or at once with no turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompactData {}

/// A compaction strategy is called; exactly one `context/compaction` with
/// `start` set to this event's sequence follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompactionStartData {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub turn_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub request_id: Option<String>,
    /// Sequence of the `context/compact` a manual step serves.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::optional_safe_integer"
    )]
    pub command: Option<u64>,
    #[serde(with = "crate::safe_integer")]
    pub through: u64,
    pub trigger: CompactionTrigger,
    pub strategy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextCompactionResult {
    /// Everything before the Timeline event `retain_from` is replaced by
    /// `replacement`, stored exactly as the strategy produced it.
    Completed {
        retain_from: String,
        replacement: Vec<Message>,
        strategy: String,
        diagnostics: Box<CompactionDiagnosticsRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        context_baseline: Vec<ContextBaselineEntry>,
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
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionData {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub turn_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub request_id: Option<String>,
    /// Sequence of the `context/compact` a manual step served.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::optional_safe_integer"
    )]
    pub command: Option<u64>,
    /// Sequence of the `context/compaction-start` this result ends; absent
    /// only for a manual command that ended before any strategy call.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::optional_safe_integer"
    )]
    pub start: Option<u64>,
    #[serde(with = "crate::safe_integer")]
    pub through: u64,
    #[serde(flatten)]
    pub outcome: ContextCompactionResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextBudgetData {
    #[serde(rename = "turn_id")]
    pub turn_id: String,
    #[serde(rename = "request_id")]
    pub request_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub boundary: Option<String>,
    #[serde(with = "crate::finite_number")]
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
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
    #[serde(with = "crate::finite_number")]
    pub context_window: f64,
    #[serde(with = "crate::finite_number")]
    pub output_tokens: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageData {
    pub turn_id: String,
    pub request_id: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCallData {
    pub turn_id: String,
    pub request_id: String,
    pub request: Arc<ModelRequest>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub configuration: Option<PromptReference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub prompt: Option<PromptReference>,
}

/// A tool call the model has started to emit; its arguments are not yet complete (ADR agent-029).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolUseStartData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolUseData {
    pub turn_id: String,
    pub request_id: String,
    pub call: ToolCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelOutputOutcome {
    Completed {
        finish_reason: FinishReason,
    },
    Failed {
        error: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        budget: Option<ProviderOutputBudgetFacts>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        presentation: Option<ProviderPresentation>,
    },
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPresentation {
    pub source: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl<'de> Deserialize<'de> for ProviderPresentation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            source: String,
            reason: String,
            #[serde(default, deserialize_with = "crate::optional_non_null")]
            server_message: Option<String>,
            #[serde(default, deserialize_with = "crate::optional_non_null")]
            request_id: Option<String>,
            #[serde(default, deserialize_with = "crate::optional_non_null")]
            action: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.source.trim().is_empty()
            || wire.reason.trim().is_empty()
            || [
                wire.server_message.as_deref(),
                wire.request_id.as_deref(),
                wire.action.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| value.trim().is_empty())
        {
            return Err(serde::de::Error::custom(
                "presentation strings must be non-empty",
            ));
        }
        Ok(Self {
            source: wire.source,
            reason: wire.reason,
            server_message: wire.server_message,
            request_id: wire.request_id,
            action: wire.action,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOutputEndData {
    pub turn_id: String,
    pub request_id: String,
    #[serde(flatten)]
    pub outcome: ModelOutputOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallData {
    pub turn_id: String,
    pub request_id: String,
    pub calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallResult {
    pub call_id: String,
    pub result: ToolResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolStartData {
    pub turn_id: String,
    pub request_id: String,
    pub call_event_id: String,
    pub call_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultData {
    pub turn_id: String,
    pub request_id: String,
    /// The dispatch this batch answers; absent when the results close calls
    /// that were never dispatched (ADR agent-029).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub call_event_id: Option<String>,
    pub results: Vec<ToolCallResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptConfigureControlData {
    pub operation_id: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnOutcome {
    Completed { reason: String },
    Failed { error: String },
    Cancelled { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnEndData {
    pub turn_id: String,
    #[serde(flatten)]
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum BranchCreateAt {
    Sequence {
        #[serde(with = "crate::safe_integer")]
        sequence: u64,
    },
    Create {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum BranchSource {
    Tool {
        turn_id: String,
        request_id: String,
        call_id: String,
    },
    Host {
        operation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchCreateData {
    pub at: BranchCreateAt,
    pub source: BranchSource,
    pub branch: String,
    pub prompt: String,
}

/// A branch begins: a child created from `parent` at `create_event`, or the
/// root of a new Session, which has neither.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchStartData {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub parent: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::optional_safe_integer"
    )]
    pub create_event: Option<u64>,
}

/// Every earlier event of this branch was copied from another Session;
/// written by the fork right after the copied facts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionForkedData {}

/// A stored branch is loaded again, for instance after a process restart.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchResumeData {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
pub enum BranchCreateFailedData {
    Failed {
        branch: String,
        #[serde(rename = "createEvent", with = "crate::safe_integer")]
        create_event: u64,
        error: String,
    },
    Cancelled {
        branch: String,
        #[serde(rename = "createEvent", with = "crate::safe_integer")]
        create_event: u64,
        reason: String,
    },
}

/// Addressed member of a `collaboration/request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum CollaborationTarget {
    /// A new member Session under an existing Agent.
    Agent { agent_id: String },
    /// A new temporary Agent and member Session.
    Temporary { profile: AgentProfile },
    /// An existing member of the requesting leader.
    Member {
        agent_id: String,
        session_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationRequestData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub target: CollaborationTarget,
    pub prompt: String,
}

/// Payload of `collaboration/cancel` and `collaboration/close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationPeerData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub agent_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationMessageMode {
    #[default]
    Append,
    Steer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationMessageData {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub mode: CollaborationMessageMode,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationErrorCode {
    InvalidInput,
    AgentNotFound,
    MemberCreateFailed,
    TeamMemberNotFound,
    Busy,
    MemberClosed,
    Interrupted,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationCancelOutcome {
    Cancelled,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
pub enum CollaborationDelivery {
    Ok {
        agent_id: String,
        session_id: String,
        /// The peer's `input/message`; request and message only.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        input_event_id: Option<String>,
        /// Cancel only.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        outcome: Option<CollaborationCancelOutcome>,
    },
    Error {
        error: CollaborationErrorCode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborationDeliveredData {
    pub op_event_id: String,
    #[serde(flatten)]
    pub delivery: CollaborationDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
pub enum CollaborationOutcome {
    Completed,
    Failed { reason: String },
    Cancelled { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborationResultData {
    pub request_event_id: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub last_turn_id: Option<String>,
    pub text: String,
    #[serde(flatten)]
    pub outcome: CollaborationOutcome,
}

/// Producer-owned data before the timeline assigns identity and ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub tags: Option<Vec<String>>,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl EventDraft {
    pub fn new(kind: EventKind) -> Self {
        Self { tags: None, kind }
    }
}
impl From<EventKind> for EventDraft {
    fn from(kind: EventKind) -> Self {
        Self::new(kind)
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelContentData {
    pub turn_id: String,
    pub request_id: String,
    pub content: Vec<MessagePart>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    InputMessage(InputMessageData),
    InputApplied(InputAppliedData),
    InputConsumed(InputConsumedData),
    InputWithdraw(InputWithdrawData),
    InputDeleted(InputDeletedData),
    StopInterrupt(StopInterruptData),
    ApprovalRequest(ApprovalRequestData),
    ApprovalRespond(ApprovalRespondData),
    ApprovalCancelled(ApprovalCancelledData),
    PermissionChanged(PermissionChangedData),
    ExecStarted(ExecStartedData),
    ExecEnded(ExecEndedData),
    TurnStart(TurnStartData),
    TurnEnd(TurnEndData),
    PromptChanged(PromptChangedData),
    PromptConfigureRequest(PromptConfigureControlData),
    PromptConfigureResult(PromptConfigureControlData),
    ContextConfigured(ContextConfiguredData),
    ContextScope(ContextScopeData),
    ContextPrepare(ContextPrepareData),
    ContextCompact(ContextCompactData),
    ContextCompactionStart(ContextCompactionStartData),
    ContextCompaction(ContextCompactionData),
    ContextBudget(ContextBudgetData),
    ModelCall(ModelCallData),
    ModelUsage(ModelUsageData),
    ModelStart(ModelStartData),
    Thinking(ThinkingData),
    Text(TextData),
    ToolUseStart(ToolUseStartData),
    ToolUse(ToolUseData),
    ModelContent(ModelContentData),
    ModelOutputEnd(ModelOutputEndData),
    ToolCall(ToolCallData),
    ToolStart(ToolStartData),
    ToolCompleted(ToolCompletedData),
    ToolResult(ToolResultData),
    BranchCreate(BranchCreateData),
    BranchStart(BranchStartData),
    BranchResume(BranchResumeData),
    BranchCreateFailed(BranchCreateFailedData),
    CollaborationRequest(CollaborationRequestData),
    CollaborationCancel(CollaborationPeerData),
    CollaborationClose(CollaborationPeerData),
    CollaborationMessage(CollaborationMessageData),
    CollaborationDelivered(CollaborationDeliveredData),
    CollaborationResult(CollaborationResultData),
    SessionForked(SessionForkedData),
    /// An event whose type the core does not define (ADR 001, issue #2).
    Extension(ExtensionEvent),
}

/// Domains (`type` values) of the events [`EventKind`] defines. They are
/// reserved for those typed events: an [`ExtensionEvent`] cannot use them.
pub const RESERVED_DOMAINS: [&str; 13] = [
    "input",
    "stop",
    "approval",
    "permission",
    "exec",
    "turn",
    "prompt",
    "context",
    "model",
    "step",
    "branch",
    "collaboration",
    "session",
];

/// An event in a domain the core does not define. Its payload is the
/// contract of the extension that introduces the type, guaranteed by its
/// producer; the core carries it unchanged. Construction checks only the
/// envelope: a non-empty domain and name without `/` or `*`, in a domain
/// outside [`RESERVED_DOMAINS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ExtensionWire", into = "ExtensionWire")]
pub struct ExtensionEvent {
    key: String,
    split: usize,
    data: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
struct ExtensionWire {
    r#type: String,
    name: String,
    data: serde_json::Value,
}

impl ExtensionEvent {
    pub fn new(
        domain: impl Into<String>,
        name: impl Into<String>,
        data: serde_json::Value,
    ) -> Result<Self, String> {
        let (domain, name) = (domain.into(), name.into());
        for (part, value) in [("domain", &domain), ("name", &name)] {
            if value.is_empty() || value.contains(['/', '*']) {
                return Err(format!(
                    "extension event {part} {value:?} must be non-empty without '/' or '*'"
                ));
            }
        }
        if RESERVED_DOMAINS.contains(&domain.as_str()) {
            return Err(format!(
                "extension event `{domain}/{name}`: domain `{domain}` is reserved for core events"
            ));
        }
        Ok(Self {
            split: domain.len(),
            key: format!("{domain}/{name}"),
            data,
        })
    }
    pub fn domain(&self) -> &str {
        &self.key[..self.split]
    }
    pub fn name(&self) -> &str {
        &self.key[self.split + 1..]
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn data(&self) -> &serde_json::Value {
        &self.data
    }
}

impl TryFrom<ExtensionWire> for ExtensionEvent {
    type Error = String;
    fn try_from(wire: ExtensionWire) -> Result<Self, String> {
        Self::new(wire.r#type, wire.name, wire.data)
    }
}

impl From<ExtensionEvent> for ExtensionWire {
    fn from(event: ExtensionEvent) -> Self {
        Self {
            r#type: event.domain().into(),
            name: event.name().into(),
            data: event.data,
        }
    }
}

impl EventKind {
    pub fn key(&self) -> &str {
        match self {
            Self::InputMessage(_) => "input/message",
            Self::InputApplied(_) => "input/applied",
            Self::InputWithdraw(_) => "input/withdraw",
            Self::InputDeleted(_) => "input/deleted",
            Self::InputConsumed(_) => "input/consumed",
            Self::StopInterrupt(_) => "stop/interrupt",
            Self::ApprovalRequest(_) => "approval/request",
            Self::ApprovalRespond(_) => "approval/respond",
            Self::ApprovalCancelled(_) => "approval/cancelled",
            Self::PermissionChanged(_) => "permission/changed",
            Self::ExecStarted(_) => "exec/started",
            Self::ExecEnded(_) => "exec/ended",
            Self::TurnStart(_) => "turn/start",
            Self::TurnEnd(_) => "turn/end",
            Self::PromptChanged(_) => "prompt/changed",
            Self::PromptConfigureRequest(_) => "prompt/configure-request",
            Self::PromptConfigureResult(_) => "prompt/configure-result",
            Self::ContextConfigured(_) => "context/configured",
            Self::ContextScope(_) => "context/scope",
            Self::ContextPrepare(_) => "context/prepare",
            Self::ContextCompact(_) => "context/compact",
            Self::ContextCompactionStart(_) => "context/compaction-start",
            Self::ContextCompaction(_) => "context/compaction",
            Self::ContextBudget(_) => "context/budget",
            Self::ModelCall(_) => "model/call",
            Self::ModelUsage(_) => "model/usage",
            Self::ModelStart(_) => "step/model-start",
            Self::Thinking(_) => "step/thinking",
            Self::Text(_) => "step/text",
            Self::ToolUseStart(_) => "step/tool-use-start",
            Self::ToolUse(_) => "step/tool-use",
            Self::ModelContent(_) => "step/model-content",
            Self::ModelOutputEnd(_) => "step/model-output-end",
            Self::ToolCall(_) => "step/tool-call",
            Self::ToolStart(_) => "step/tool-start",
            Self::ToolCompleted(_) => "step/tool-completed",
            Self::ToolResult(_) => "step/tool-result",
            Self::BranchCreate(_) => "branch/create",
            Self::BranchStart(_) => "branch/start",
            Self::BranchResume(_) => "branch/resume",
            Self::BranchCreateFailed(_) => "branch/create-failed",
            Self::CollaborationRequest(_) => "collaboration/request",
            Self::CollaborationCancel(_) => "collaboration/cancel",
            Self::CollaborationClose(_) => "collaboration/close",
            Self::CollaborationMessage(_) => "collaboration/message",
            Self::CollaborationDelivered(_) => "collaboration/delivered",
            Self::CollaborationResult(_) => "collaboration/result",
            Self::SessionForked(_) => "session/forked",
            Self::Extension(event) => event.key(),
        }
    }

    /// Internal timeline controls are absent from the version 1 wire event union.
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            Self::StopInterrupt(_)
                | Self::ApprovalRequest(_)
                | Self::ApprovalRespond(_)
                | Self::ApprovalCancelled(_)
                | Self::PromptConfigureRequest(_)
                | Self::PromptConfigureResult(_)
        )
    }
}

// A core event by domain: each domain's nested tagged enum keeps typed payload
// validation and serde's field errors without converting payloads through
// serde_json::Value.
enum TaggedEvent {
    Input(InputEvent),
    Stop(StopEvent),
    Approval(ApprovalEvent),
    Permission(PermissionEvent),
    Exec(ExecEvent),
    Turn(TurnEvent),
    Prompt(PromptEvent),
    Context(ContextEvent),
    Model(ModelEvent),
    Step(StepEvent),
    Branch(BranchEvent),
    Collaboration(CollaborationEvent),
    Session(SessionEvent),
}

impl TaggedEvent {
    /// Decodes the rest of an event whose `type` is the core domain `domain`.
    fn deserialize<'de, D: serde::Deserializer<'de>>(
        domain: &str,
        rest: D,
    ) -> Result<Self, D::Error> {
        Ok(match domain {
            "input" => Self::Input(Deserialize::deserialize(rest)?),
            "stop" => Self::Stop(Deserialize::deserialize(rest)?),
            "approval" => Self::Approval(Deserialize::deserialize(rest)?),
            "permission" => Self::Permission(Deserialize::deserialize(rest)?),
            "exec" => Self::Exec(Deserialize::deserialize(rest)?),
            "turn" => Self::Turn(Deserialize::deserialize(rest)?),
            "prompt" => Self::Prompt(Deserialize::deserialize(rest)?),
            "context" => Self::Context(Deserialize::deserialize(rest)?),
            "model" => Self::Model(Deserialize::deserialize(rest)?),
            "step" => Self::Step(Deserialize::deserialize(rest)?),
            "branch" => Self::Branch(Deserialize::deserialize(rest)?),
            "collaboration" => Self::Collaboration(Deserialize::deserialize(rest)?),
            "session" => Self::Session(Deserialize::deserialize(rest)?),
            _ => return Err(de::Error::unknown_variant(domain, &RESERVED_DOMAINS)),
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum SessionEvent {
    #[serde(rename = "forked")]
    Forked(SessionForkedData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum InputEvent {
    #[serde(rename = "message")]
    InputMessage(InputMessageData),
    #[serde(rename = "applied")]
    InputApplied(InputAppliedData),
    #[serde(rename = "consumed")]
    InputConsumed(InputConsumedData),
    #[serde(rename = "withdraw")]
    InputWithdraw(InputWithdrawData),
    #[serde(rename = "deleted")]
    InputDeleted(InputDeletedData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum StopEvent {
    #[serde(rename = "interrupt")]
    Interrupt(StopInterruptData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum ApprovalEvent {
    #[serde(rename = "request")]
    Request(ApprovalRequestData),
    #[serde(rename = "respond")]
    Respond(ApprovalRespondData),
    #[serde(rename = "cancelled")]
    Cancelled(ApprovalCancelledData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum PermissionEvent {
    #[serde(rename = "changed")]
    Changed(PermissionChangedData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum ExecEvent {
    #[serde(rename = "started")]
    Started(ExecStartedData),
    #[serde(rename = "ended")]
    Ended(ExecEndedData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum TurnEvent {
    #[serde(rename = "start")]
    TurnStart(TurnStartData),
    #[serde(rename = "end")]
    TurnEnd(TurnEndData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum ContextEvent {
    #[serde(rename = "configured")]
    Configured(ContextConfiguredData),
    #[serde(rename = "scope")]
    Scope(ContextScopeData),
    #[serde(rename = "prepare")]
    Prepare(ContextPrepareData),
    #[serde(rename = "compact")]
    Compact(ContextCompactData),
    #[serde(rename = "compaction-start")]
    CompactionStart(ContextCompactionStartData),
    #[serde(rename = "compaction")]
    Compaction(ContextCompactionData),
    #[serde(rename = "budget")]
    Budget(ContextBudgetData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum PromptEvent {
    #[serde(rename = "changed")]
    Changed(PromptChangedData),
    #[serde(rename = "configure-request")]
    ConfigureRequest(PromptConfigureControlData),
    #[serde(rename = "configure-result")]
    ConfigureResult(PromptConfigureControlData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum ModelEvent {
    #[serde(rename = "call")]
    ModelCall(ModelCallData),
    #[serde(rename = "usage")]
    ModelUsage(ModelUsageData),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
pub enum StepEvent {
    #[serde(rename = "model-start")]
    ModelStart(ModelStartData),
    #[serde(rename = "thinking")]
    Thinking(ThinkingData),
    #[serde(rename = "text")]
    Text(TextData),
    #[serde(rename = "tool-use-start")]
    ToolUseStart(ToolUseStartData),
    #[serde(rename = "tool-use")]
    ToolUse(ToolUseData),
    #[serde(rename = "model-content")]
    ModelContent(ModelContentData),
    #[serde(rename = "model-output-end")]
    ModelOutputEnd(ModelOutputEndData),
    #[serde(rename = "tool-call")]
    ToolCall(ToolCallData),
    #[serde(rename = "tool-start")]
    ToolStart(ToolStartData),
    #[serde(rename = "tool-completed")]
    ToolCompleted(ToolCompletedData),
    #[serde(rename = "tool-result")]
    ToolResult(ToolResultData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum BranchEvent {
    #[serde(rename = "create")]
    Create(BranchCreateData),
    #[serde(rename = "start")]
    Start(BranchStartData),
    #[serde(rename = "resume")]
    Resume(BranchResumeData),
    #[serde(rename = "create-failed")]
    CreateFailed(BranchCreateFailedData),
}

#[derive(Deserialize)]
#[serde(tag = "name", content = "data", deny_unknown_fields)]
enum CollaborationEvent {
    #[serde(rename = "request")]
    Request(CollaborationRequestData),
    #[serde(rename = "cancel")]
    Cancel(CollaborationPeerData),
    #[serde(rename = "close")]
    Close(CollaborationPeerData),
    #[serde(rename = "message")]
    Message(CollaborationMessageData),
    #[serde(rename = "delivered")]
    Delivered(CollaborationDeliveredData),
    #[serde(rename = "result")]
    Result(CollaborationResultData),
}

// The domain picks the decoder before the payload is read: a reserved domain
// decodes only through its typed contract, so a malformed core event fails
// with serde's own error; any other domain is an extension event whose data is
// kept as given. Entries before `type` are held until then; events as written
// put `type` before `name` and `data`, so none are.
impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(EventVisitor)
    }
}

struct EventVisitor;

impl<'de> de::Visitor<'de> for EventVisitor {
    type Value = EventKind;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an event with `type`, `name` and `data`")
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<EventKind, A::Error> {
        use de::Error;
        let (mut domain, mut name, mut data) = (None::<String>, None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" if domain.is_none() => {
                    let value: String = map.next_value()?;
                    if RESERVED_DOMAINS.contains(&value.as_str()) {
                        let rest = Rest {
                            held: [
                                name.map(|name| ("name", serde_json::Value::String(name))),
                                data.map(|data| ("data", data)),
                            ],
                            value: None,
                            map,
                        };
                        return TaggedEvent::deserialize(
                            &value,
                            de::value::MapAccessDeserializer::new(rest),
                        )
                        .map(EventKind::from);
                    }
                    domain = Some(value);
                }
                "name" if name.is_none() => name = Some(map.next_value::<String>()?),
                "data" if data.is_none() => data = Some(map.next_value::<serde_json::Value>()?),
                "type" => return Err(A::Error::duplicate_field("type")),
                "name" => return Err(A::Error::duplicate_field("name")),
                "data" => return Err(A::Error::duplicate_field("data")),
                _ => return Err(A::Error::unknown_field(&key, &["type", "name", "data"])),
            }
        }
        ExtensionEvent::new(
            domain.ok_or_else(|| A::Error::missing_field("type"))?,
            name.ok_or_else(|| A::Error::missing_field("name"))?,
            data.ok_or_else(|| A::Error::missing_field("data"))?,
        )
        .map(EventKind::Extension)
        .map_err(A::Error::custom)
    }
}

/// The rest of a core event after its `type`: the entries held before it,
/// `name` first so the tag picks the payload type, then the map's own.
struct Rest<A> {
    held: [Option<(&'static str, serde_json::Value)>; 2],
    value: Option<serde_json::Value>,
    map: A,
}

impl<'de, A: de::MapAccess<'de>> de::MapAccess<'de> for Rest<A> {
    type Error = A::Error;
    fn next_key_seed<K: de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        let Some((key, value)) = self.held.iter_mut().find_map(Option::take) else {
            return self.map.next_key_seed(seed);
        };
        self.value = Some(value);
        seed.deserialize(de::value::StrDeserializer::new(key))
            .map(Some)
    }
    fn next_value_seed<V: de::DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, A::Error> {
        match self.value.take() {
            Some(value) => seed.deserialize(value).map_err(de::Error::custom),
            None => self.map.next_value_seed(seed),
        }
    }
}

impl From<TaggedEvent> for EventKind {
    fn from(event: TaggedEvent) -> Self {
        match event {
            TaggedEvent::Input(InputEvent::InputMessage(data)) => Self::InputMessage(data),
            TaggedEvent::Input(InputEvent::InputApplied(data)) => Self::InputApplied(data),
            TaggedEvent::Input(InputEvent::InputWithdraw(data)) => Self::InputWithdraw(data),
            TaggedEvent::Input(InputEvent::InputDeleted(data)) => Self::InputDeleted(data),
            TaggedEvent::Input(InputEvent::InputConsumed(data)) => Self::InputConsumed(data),
            TaggedEvent::Stop(StopEvent::Interrupt(data)) => Self::StopInterrupt(data),
            TaggedEvent::Approval(ApprovalEvent::Request(data)) => Self::ApprovalRequest(data),
            TaggedEvent::Approval(ApprovalEvent::Respond(data)) => Self::ApprovalRespond(data),
            TaggedEvent::Approval(ApprovalEvent::Cancelled(data)) => Self::ApprovalCancelled(data),
            TaggedEvent::Permission(PermissionEvent::Changed(data)) => {
                Self::PermissionChanged(data)
            }
            TaggedEvent::Exec(ExecEvent::Started(data)) => Self::ExecStarted(data),
            TaggedEvent::Exec(ExecEvent::Ended(data)) => Self::ExecEnded(data),
            TaggedEvent::Turn(TurnEvent::TurnStart(data)) => Self::TurnStart(data),
            TaggedEvent::Turn(TurnEvent::TurnEnd(data)) => Self::TurnEnd(data),
            TaggedEvent::Prompt(PromptEvent::Changed(data)) => Self::PromptChanged(data),
            TaggedEvent::Prompt(PromptEvent::ConfigureRequest(data)) => {
                Self::PromptConfigureRequest(data)
            }
            TaggedEvent::Prompt(PromptEvent::ConfigureResult(data)) => {
                Self::PromptConfigureResult(data)
            }
            TaggedEvent::Context(ContextEvent::Configured(data)) => Self::ContextConfigured(data),
            TaggedEvent::Context(ContextEvent::Scope(data)) => Self::ContextScope(data),
            TaggedEvent::Context(ContextEvent::Prepare(data)) => Self::ContextPrepare(data),
            TaggedEvent::Context(ContextEvent::Compact(data)) => Self::ContextCompact(data),
            TaggedEvent::Context(ContextEvent::CompactionStart(data)) => {
                Self::ContextCompactionStart(data)
            }
            TaggedEvent::Context(ContextEvent::Compaction(data)) => Self::ContextCompaction(data),
            TaggedEvent::Context(ContextEvent::Budget(data)) => Self::ContextBudget(data),
            TaggedEvent::Model(ModelEvent::ModelCall(data)) => Self::ModelCall(data),
            TaggedEvent::Model(ModelEvent::ModelUsage(data)) => Self::ModelUsage(data),
            TaggedEvent::Step(StepEvent::ModelStart(data)) => Self::ModelStart(data),
            TaggedEvent::Step(StepEvent::Thinking(data)) => Self::Thinking(data),
            TaggedEvent::Step(StepEvent::Text(data)) => Self::Text(data),
            TaggedEvent::Step(StepEvent::ToolUseStart(data)) => Self::ToolUseStart(data),
            TaggedEvent::Step(StepEvent::ToolUse(data)) => Self::ToolUse(data),
            TaggedEvent::Step(StepEvent::ModelContent(data)) => Self::ModelContent(data),
            TaggedEvent::Step(StepEvent::ModelOutputEnd(data)) => Self::ModelOutputEnd(data),
            TaggedEvent::Step(StepEvent::ToolCall(data)) => Self::ToolCall(data),
            TaggedEvent::Step(StepEvent::ToolStart(data)) => Self::ToolStart(data),
            TaggedEvent::Step(StepEvent::ToolCompleted(data)) => Self::ToolCompleted(data),
            TaggedEvent::Step(StepEvent::ToolResult(data)) => Self::ToolResult(data),
            TaggedEvent::Branch(BranchEvent::Create(data)) => Self::BranchCreate(data),
            TaggedEvent::Branch(BranchEvent::Start(data)) => Self::BranchStart(data),
            TaggedEvent::Branch(BranchEvent::Resume(data)) => Self::BranchResume(data),
            TaggedEvent::Branch(BranchEvent::CreateFailed(data)) => Self::BranchCreateFailed(data),
            TaggedEvent::Collaboration(CollaborationEvent::Request(data)) => {
                Self::CollaborationRequest(data)
            }
            TaggedEvent::Collaboration(CollaborationEvent::Cancel(data)) => {
                Self::CollaborationCancel(data)
            }
            TaggedEvent::Collaboration(CollaborationEvent::Close(data)) => {
                Self::CollaborationClose(data)
            }
            TaggedEvent::Collaboration(CollaborationEvent::Message(data)) => {
                Self::CollaborationMessage(data)
            }
            TaggedEvent::Collaboration(CollaborationEvent::Delivered(data)) => {
                Self::CollaborationDelivered(data)
            }
            TaggedEvent::Collaboration(CollaborationEvent::Result(data)) => {
                Self::CollaborationResult(data)
            }
            TaggedEvent::Session(SessionEvent::Forked(data)) => Self::SessionForked(data),
        }
    }
}

impl Serialize for EventKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("EventKind", 3)?;
        match self {
            Self::InputMessage(data) => {
                state.serialize_field("type", "input")?;
                state.serialize_field("name", "message")?;
                state.serialize_field("data", data)?;
            }
            Self::InputApplied(data) => {
                state.serialize_field("type", "input")?;
                state.serialize_field("name", "applied")?;
                state.serialize_field("data", data)?;
            }
            Self::InputConsumed(data) => {
                state.serialize_field("type", "input")?;
                state.serialize_field("name", "consumed")?;
                state.serialize_field("data", data)?;
            }
            Self::InputWithdraw(data) => {
                state.serialize_field("type", "input")?;
                state.serialize_field("name", "withdraw")?;
                state.serialize_field("data", data)?;
            }
            Self::InputDeleted(data) => {
                state.serialize_field("type", "input")?;
                state.serialize_field("name", "deleted")?;
                state.serialize_field("data", data)?;
            }
            Self::StopInterrupt(data) => {
                state.serialize_field("type", "stop")?;
                state.serialize_field("name", "interrupt")?;
                state.serialize_field("data", data)?;
            }
            Self::ApprovalRequest(data) => {
                state.serialize_field("type", "approval")?;
                state.serialize_field("name", "request")?;
                state.serialize_field("data", data)?;
            }
            Self::ApprovalRespond(data) => {
                state.serialize_field("type", "approval")?;
                state.serialize_field("name", "respond")?;
                state.serialize_field("data", data)?;
            }
            Self::ApprovalCancelled(data) => {
                state.serialize_field("type", "approval")?;
                state.serialize_field("name", "cancelled")?;
                state.serialize_field("data", data)?;
            }
            Self::PermissionChanged(data) => {
                state.serialize_field("type", "permission")?;
                state.serialize_field("name", "changed")?;
                state.serialize_field("data", data)?;
            }
            Self::ExecStarted(data) => {
                state.serialize_field("type", "exec")?;
                state.serialize_field("name", "started")?;
                state.serialize_field("data", data)?;
            }
            Self::ExecEnded(data) => {
                state.serialize_field("type", "exec")?;
                state.serialize_field("name", "ended")?;
                state.serialize_field("data", data)?;
            }
            Self::TurnStart(data) => {
                state.serialize_field("type", "turn")?;
                state.serialize_field("name", "start")?;
                state.serialize_field("data", data)?;
            }
            Self::TurnEnd(data) => {
                state.serialize_field("type", "turn")?;
                state.serialize_field("name", "end")?;
                state.serialize_field("data", data)?;
            }
            Self::PromptChanged(data) => {
                state.serialize_field("type", "prompt")?;
                state.serialize_field("name", "changed")?;
                state.serialize_field("data", data)?;
            }
            Self::PromptConfigureRequest(data) => {
                state.serialize_field("type", "prompt")?;
                state.serialize_field("name", "configure-request")?;
                state.serialize_field("data", data)?;
            }
            Self::PromptConfigureResult(data) => {
                state.serialize_field("type", "prompt")?;
                state.serialize_field("name", "configure-result")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextConfigured(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "configured")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextScope(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "scope")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextPrepare(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "prepare")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextCompact(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "compact")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextCompactionStart(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "compaction-start")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextCompaction(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "compaction")?;
                state.serialize_field("data", data)?;
            }
            Self::ContextBudget(data) => {
                state.serialize_field("type", "context")?;
                state.serialize_field("name", "budget")?;
                state.serialize_field("data", data)?;
            }
            Self::ModelCall(data) => {
                state.serialize_field("type", "model")?;
                state.serialize_field("name", "call")?;
                state.serialize_field("data", data)?;
            }
            Self::ModelUsage(data) => {
                state.serialize_field("type", "model")?;
                state.serialize_field("name", "usage")?;
                state.serialize_field("data", data)?;
            }
            Self::ModelStart(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "model-start")?;
                state.serialize_field("data", data)?;
            }
            Self::Thinking(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "thinking")?;
                state.serialize_field("data", data)?;
            }
            Self::Text(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "text")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolUseStart(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-use-start")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolUse(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-use")?;
                state.serialize_field("data", data)?;
            }
            Self::ModelContent(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "model-content")?;
                state.serialize_field("data", data)?;
            }
            Self::ModelOutputEnd(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "model-output-end")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolCall(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-call")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolStart(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-start")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolCompleted(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-completed")?;
                state.serialize_field("data", data)?;
            }
            Self::ToolResult(data) => {
                state.serialize_field("type", "step")?;
                state.serialize_field("name", "tool-result")?;
                state.serialize_field("data", data)?;
            }
            Self::BranchCreate(data) => {
                state.serialize_field("type", "branch")?;
                state.serialize_field("name", "create")?;
                state.serialize_field("data", data)?;
            }
            Self::BranchStart(data) => {
                state.serialize_field("type", "branch")?;
                state.serialize_field("name", "start")?;
                state.serialize_field("data", data)?;
            }
            Self::BranchResume(data) => {
                state.serialize_field("type", "branch")?;
                state.serialize_field("name", "resume")?;
                state.serialize_field("data", data)?;
            }
            Self::BranchCreateFailed(data) => {
                state.serialize_field("type", "branch")?;
                state.serialize_field("name", "create-failed")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationRequest(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "request")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationCancel(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "cancel")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationClose(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "close")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationMessage(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "message")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationDelivered(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "delivered")?;
                state.serialize_field("data", data)?;
            }
            Self::CollaborationResult(data) => {
                state.serialize_field("type", "collaboration")?;
                state.serialize_field("name", "result")?;
                state.serialize_field("data", data)?;
            }
            Self::SessionForked(data) => {
                state.serialize_field("type", "session")?;
                state.serialize_field("name", "forked")?;
                state.serialize_field("data", data)?;
            }
            Self::Extension(event) => {
                state.serialize_field("type", event.domain())?;
                state.serialize_field("name", event.name())?;
                state.serialize_field("data", event.data())?;
            }
        }
        state.end()
    }
}
