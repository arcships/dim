//! Public RPC v3 product-event and progress boundary types.
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
    pub content: EventContent,
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
