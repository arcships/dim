//! Wire types of the App Server session notifications.
//!
//! These are decoded once at the boundary and handed to the projection and
//! the scrollback printer as plain values. Nothing else parses these shapes.

use dim_agent_types::*;
use serde::Deserialize;

pub struct Event {
    pub sequence: u64,
    pub content: EventContent,
}
pub enum EventContent {
    Input(InputMessageData),
    TurnEnd(TurnEndData),
    Thinking(ThinkingData),
    Text(TextData),
    ToolUse(ToolUseData),
    ToolCompleted(Box<ToolCompletedData>),
    ToolResult(Box<ToolResultData>),
    ModelEnd(Box<ModelOutputEndData>),
    Other,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            sequence: u64,
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
            ("input", "message") => EventContent::Input(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("turn", "end") => EventContent::TurnEnd(
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
            ("step", "tool-completed") => EventContent::ToolCompleted(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-result") => EventContent::ToolResult(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "model-output-end") => EventContent::ModelEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            _ => EventContent::Other,
        };
        Ok(Self {
            sequence: wire.sequence,
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
}
