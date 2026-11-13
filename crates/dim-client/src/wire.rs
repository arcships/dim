//! Public RPC product-event and progress boundary types.
//!
//! These are decoded once at the boundary and handed to the projection and
//! the scrollback printer as plain values. Nothing else parses these shapes.

use crate::Result;
use dim_agent_types::*;
use serde::Deserialize;
use serde_json::Value;

pub struct Event {
    /// The Timeline's event id, used by held-input and delegation controls.
    pub id: String,
    pub sequence: u64,
    /// Unix milliseconds the runtime wrote the event at.
    pub create_at: u64,
    /// v4 only: the outer call of a child tool batch.
    pub parent_call_id: Option<String>,
    pub content: EventContent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNotice {
    pub session_id: String,
    pub branch_id: String,
    pub event: Event,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayNotice {
    pub session_id: String,
    pub branch_id: String,
    pub item: Event,
}

impl From<DisplayNotice> for EventNotice {
    fn from(notice: DisplayNotice) -> Self {
        Self {
            session_id: notice.session_id,
            branch_id: notice.branch_id,
            event: notice.item,
        }
    }
}
#[derive(Clone, Deserialize)]
pub struct Configuration {
    pub provider_id: String,
    pub model_id: String,
    /// The adopted model's capability declaration, including temporary routes.
    #[serde(default)]
    pub capability_snapshot: Option<Box<ModelCapabilitySnapshot>>,
    /// The effective preferences the adopted configuration sends; absent when
    /// it carries none.
    #[serde(default)]
    pub model_preferences: Option<dim_agent_types::ModelPreferences>,
}
pub enum EventContent {
    BranchCreate(BranchCreateData),
    BranchStart(BranchStartData),
    BranchCreateFailed(BranchCreateFailedData),
    BranchEnd(BranchEndData),
    BranchEnded(BranchEndedData),
    /// The fork's own opening: the facts before it are copied from the parent.
    SessionForked(SessionForkedData),
    Configuration(Configuration),
    Input(InputMessageData),
    /// InputQueue released the input to the turn.
    InputApplied(InputAppliedData),
    /// InputQueue dropped a held input, on withdrawal.
    InputDeleted(InputDeletedData),
    TurnEnd(TurnEndData),
    /// A command moved from the `exec` call to the background.
    ExecStarted(ExecStartedData),
    /// A background command ended.
    ExecEnded(ExecEndedData),
    /// The leader delegated a task to a member agent.
    CollaborationRequest(CollaborationRequestData),
    /// Delivery binds a delegation's request event to its actual member.
    CollaborationDelivered(CollaborationDeliveredData),
    /// A member's final result reached the leader.
    CollaborationResult(CollaborationResultData),
    /// The branch's permission policy changed, from any client.
    PermissionChanged(PermissionChangedData),
    ModelStart(ModelStartData),
    Thinking(ThinkingData),
    Text(TextData),
    ToolUse(ToolUseData),
    ToolCall(Box<ToolCallData>),
    ToolCompleted(Box<ToolCompletedData>),
    ToolResult(Box<ToolResultData>),
    ModelEnd(Box<ModelOutputEndData>),
    InputConsumed(InputConsumedData),
    InputWithdraw(InputWithdrawData),
    TurnStart(TurnStartData),
    ModelUsage(ModelUsageData),
    ToolUseStart(ToolUseStartData),
    ToolStart(ToolStartData),
    CompactionStart(ContextCompactionStartData),
    Compaction(ContextCompactionData),
    CompactionSummary(CompactionSummary),
    Other,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            id: String,
            sequence: u64,
            create_at: u64,
            #[serde(default)]
            parent_call_id: Option<String>,
            r#type: String,
            name: String,
            data: serde_json::Value,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.sequence > JS_MAX_SAFE_INTEGER {
            return Err(serde::de::Error::custom(
                "Event sequence exceeds the protocol range",
            ));
        }
        let content = match (wire.r#type.as_str(), wire.name.as_str()) {
            ("display", "compaction") => EventContent::CompactionSummary(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("branch", "create") => EventContent::BranchCreate(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("branch", "start") => EventContent::BranchStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("branch", "create-failed") => EventContent::BranchCreateFailed(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("branch", "end") => EventContent::BranchEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("branch", "ended") => EventContent::BranchEnded(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("session", "forked") => EventContent::SessionForked(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("context", "configured") => EventContent::Configuration(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("input", "message") => EventContent::Input(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("input", "applied") => EventContent::InputApplied(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("input", "deleted") => EventContent::InputDeleted(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("turn", "end") => EventContent::TurnEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("exec", "started") => EventContent::ExecStarted(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("exec", "ended") => EventContent::ExecEnded(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("collaboration", "request") => EventContent::CollaborationRequest(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("collaboration", "delivered") => EventContent::CollaborationDelivered(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("collaboration", "result") => EventContent::CollaborationResult(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("permission", "changed") => EventContent::PermissionChanged(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "model-start") => EventContent::ModelStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "thinking") => EventContent::Thinking(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "text") => EventContent::Text(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-use") => EventContent::ToolUse(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-call") => EventContent::ToolCall(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-completed") => EventContent::ToolCompleted(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-result") => EventContent::ToolResult(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "model-output-end") => EventContent::ModelEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("input", "consumed") => EventContent::InputConsumed(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("input", "withdraw") => EventContent::InputWithdraw(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("turn", "start") => EventContent::TurnStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("model", "usage") => EventContent::ModelUsage(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-use-start") => EventContent::ToolUseStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-start") => EventContent::ToolStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("context", "compaction-start") => EventContent::CompactionStart(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("context", "compaction") => EventContent::Compaction(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            _ => EventContent::Other,
        };
        Ok(Self {
            id: wire.id,
            sequence: wire.sequence,
            create_at: wire.create_at,
            parent_call_id: wire.parent_call_id,
            content,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: Option<String>,
    pub text: String,
    pub text_offset: u64,
    /// Reasoning not written to the Timeline yet: it follows the first
    /// `thinking_offset` characters of the request's `step/thinking` deltas.
    pub thinking: String,
    pub thinking_offset: u64,
    /// Unix milliseconds at which the open thinking block started; present
    /// while a thinking block is open.
    pub thinking_started_at: Option<u64>,
    /// The retry this request is waiting to make, while it sleeps.
    pub retry: Option<RetryProgress>,
}
/// A retry a request is waiting for, shown while it sleeps.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryProgress {
    /// The attempt about to run, at least 2.
    pub attempt: u8,
    /// The total cap on attempts.
    pub max_attempts: u8,
    /// Unix milliseconds at which the attempt starts.
    pub retry_at_ms: u64,
    pub kind: ModelFailureKind,
    /// The failure's bounded reason.
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub events: Vec<Value>,
    pub through: u64,
    next_after: u64,
    done: bool,
}
impl History {
    /// Checks a page read from `after` against the read's fixed watermark
    /// and returns the next cursor, or `None` when the read is complete.
    pub fn next(&self, after: u64, through: Option<u64>) -> Result<Option<u64>> {
        if through.is_some_and(|through| through != self.through)
            || self.through > dim_agent_types::JS_MAX_SAFE_INTEGER
            || self.next_after < after
            || self.next_after > self.through
            || !self.done && self.next_after == after
        {
            return Err("Invalid history page cursor".into());
        }
        for event in &self.events {
            let sequence = event["sequence"]
                .as_u64()
                .ok_or("Invalid history event sequence")?;
            if sequence <= after || sequence > self.next_after {
                return Err("History event is outside its page".into());
            }
        }
        if !self.done {
            return Ok(Some(self.next_after));
        }
        if self.next_after != self.through {
            return Err("History ended before its fixed watermark".into());
        }
        Ok(None)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryWindow {
    pub events: Vec<Value>,
    pub through: u64,
    pub next_before: Option<u64>,
    next_after: u64,
}

/// A display page contains complete product events or explicit display summaries.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayPage {
    pub items: Vec<Value>,
    pub through: u64,
    pub next_before: Option<u64>,
    next_after: u64,
    done: bool,
}

impl From<DisplayPage> for HistoryWindow {
    fn from(page: DisplayPage) -> Self {
        Self {
            events: page.items,
            through: page.through,
            next_before: page.next_before,
            next_after: page.next_after,
        }
    }
}

impl From<DisplayPage> for History {
    fn from(page: DisplayPage) -> Self {
        Self {
            events: page.items,
            through: page.through,
            next_after: page.next_after,
            done: page.done,
        }
    }
}

impl HistoryWindow {
    /// Validates a backward page against its exclusive upper bound and fixed
    /// cutoff. A latest-turn boundary does not end the branch's history.
    pub fn next(&self, before: Option<u64>, through: Option<u64>) -> Result<Option<u64>> {
        if self.through > JS_MAX_SAFE_INTEGER
            || through.is_some_and(|head| head != self.through)
            || self.next_after > self.through
            || self.next_before.is_some_and(|first| {
                first == 0
                    || first > self.next_after
                    || before.is_some_and(|before| first >= before)
            })
            || !self.events.is_empty() && self.next_before.is_none()
        {
            return Err("Invalid backward history cursor".into());
        }
        let mut previous = None;
        for event in &self.events {
            let sequence = event["sequence"]
                .as_u64()
                .ok_or("Invalid history event sequence")?;
            if previous.is_some_and(|previous| sequence <= previous)
                || self.next_before.is_some_and(|first| sequence < first)
                || sequence > self.next_after
                || before.is_some_and(|before| sequence >= before)
            {
                return Err("History event is outside its backward page".into());
            }
            previous = Some(sequence);
        }
        Ok(self.next_before.filter(|before| *before > 1))
    }
}

/// The record `session.attach` replies for the Session it opened. A server
/// from before the record existed replies `{}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub model: Option<dim_agent_types::ModelId>,
    #[serde(default)]
    pub capability_snapshot: Option<Box<dim_agent_types::ModelCapabilitySnapshot>>,
    /// The adopted configuration's preferences; absent when it carries none.
    #[serde(default)]
    pub model_preferences: Option<dim_agent_types::ModelPreferences>,
    /// The Session's workspace; absent from servers older than the record.
    #[serde(default)]
    pub cwd: Option<String>,
    /// The Session's stored title; absent from servers older than the title
    /// fields, and `None` when it has none.
    #[serde(default)]
    pub title: Option<String>,
    /// The title's revision; 0 on a server that reports none.
    #[serde(default)]
    pub title_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingQuestions {
    pub branch_id: String,
    pub through: u64,
    pub batches: Vec<dim_agent_types::PendingQuestionBatch>,
    pub next: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputs {
    pub branch_id: String,
    pub through: u64,
    pub inputs: Vec<dim_agent_types::PendingInput>,
    pub next: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTasks {
    pub branch_id: String,
    pub through: u64,
    pub tasks: Vec<dim_agent_types::PendingExec>,
    pub next: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDelegations {
    pub branch_id: String,
    pub through: u64,
    pub requests: Vec<dim_agent_types::PendingDelegation>,
    pub next: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRead {
    pub branch_id: String,
    pub through: u64,
    pub turn: Option<TurnBoundary>,
    pub output: Option<dim_agent_types::ActiveOutput>,
    #[serde(default)]
    pub input_deleted: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBoundary {
    pub turn_id: String,
    pub sequence: u64,
    pub ended: bool,
    #[serde(default)]
    pub outcome: Option<dim_agent_types::TurnOutcome>,
}

#[derive(Deserialize)]
pub struct BranchDirectory {
    pub branches: Vec<BranchEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchEntry {
    pub purpose: Option<String>,
    pub cut_messages: Option<u64>,
    pub end: Option<dim_agent_types::BranchEndData>,
    pub branch_id: String,
    pub parent_branch_id: Option<String>,
    pub branch_point: Option<u64>,
    pub state: String,
    pub head: u64,
}

#[derive(Deserialize)]
pub struct MethodTable {
    pub methods: Vec<MethodDeclaration>,
}
#[derive(Deserialize)]
pub struct MethodDeclaration {
    pub name: String,
    pub mutating: bool,
}
