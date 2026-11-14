use crate::JsonObject;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceType {
    Base64,
    Url,
    File,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(rename = "type")]
    pub source_type: ImageSourceType,
    pub media_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub data: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl From<String> for ToolContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for ToolContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

/// Native continuation facts preserved at the model boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ContentMetadata {
    Bedrock {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        signature: Option<String>,
    },
    BedrockRedacted {
        data: String,
    },
    Anthropic {
        signature: String,
    },
    AnthropicRedacted {
        data: String,
    },
    Gemini {
        #[serde(rename = "thoughtSignature")]
        thought_signature: String,
    },
    XaiResponses {
        #[serde(rename = "itemId")]
        item_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null",
            rename = "encryptedContent"
        )]
        encrypted_content: Option<String>,
    },
    OpenaiResponses {
        #[serde(rename = "itemId")]
        item_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        #[serde(rename = "encryptedContent")]
        encrypted_content: Option<String>,
        // Sessions written before the aimux v0.6.0 upgrade carry this index.
        // The provider no longer emits it, and the Responses converter ignores
        // the key when it replays a request: the field stays readable for those
        // sessions, but nothing writes it and nothing puts it on the wire.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        #[serde(rename = "summaryIndex")]
        summary_index: Option<u32>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        phase: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessagePart {
    Text {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        #[serde(rename = "providerMetadata")]
        provider_metadata: Option<ContentMetadata>,
        text: String,
        #[serde(
            rename = "startTime",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        start_time: Option<String>,
        #[serde(
            rename = "endTime",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        end_time: Option<String>,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        #[serde(rename = "providerMetadata")]
        provider_metadata: Option<ContentMetadata>,
        id: String,
        name: String,
        input: JsonObject,
        #[serde(
            rename = "startTime",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        start_time: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolContent,
        #[serde(
            rename = "structuredContent",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        structured_content: Option<JsonObject>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        is_error: Option<bool>,
    },
    Thinking {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        #[serde(rename = "providerMetadata")]
        provider_metadata: Option<ContentMetadata>,
        thinking: String,
        #[serde(
            rename = "startTime",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        start_time: Option<String>,
        #[serde(
            rename = "endTime",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        end_time: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    ToolResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: MessageContent::Text(text.into()),
        }
    }
}
