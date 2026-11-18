//! Deterministic in-process hosts for prototype and transport tests.
use crate::{host::*, tool_registry::definition};
use dim_agent_runtime::{Names, SessionConfig};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Maximum UTF-8 text or serialized tool-call bytes emitted by a mock host.
/// Event envelopes and JSON escaping of text are accounted for separately.
pub const MAX_MOCK_OUTPUT_BYTES: usize = 1024 * 1024;

pub struct EchoModel;
impl EchoModel {
    /// Selects the latest mock command or tool result, skipping runtime state and rules.
    pub fn last_message(request: &ModelRequest) -> Option<&Message> {
        request.messages.iter().rev().map(AsRef::as_ref).find(|message: &&Message| {
            message.role != Role::User || !match &message.content {
                MessageContent::Text(text) => crate::context_block::is_state_or_rule(text),
                MessageContent::Parts(parts) => matches!(
                    parts.as_slice(),
                    [MessagePart::Text { text, .. }] if crate::context_block::is_state_or_rule(text)
                ),
            }
        })
    }
}
impl ModelDriver for EchoModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: crate::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            crate::context_policy::check_cancel(&cancel)?;
            let id = uuid::Uuid::new_v4().to_string();
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let last = Self::last_message(&request);
            let text = last.map(message_text).transpose()?.unwrap_or_default();
            let text = delegated_message_body(&text).unwrap_or(text);
            let reason = if last.is_some_and(|message| message.role == Role::User)
                && text.starts_with("tool:")
            {
                let command = &text[5..];
                let (name, input) = command.split_once(' ').unwrap_or((command, "{}"));
                let input: JsonObject = serde_json::from_str(input)
                    .map_err(|_| HostError::failed("Invalid mock tool command JSON"))?;
                let call = ToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: name.into(),
                    input,
                };
                if serde_json::to_vec(&call)
                    .expect("ToolCall contains JSON data")
                    .len()
                    > MAX_MOCK_OUTPUT_BYTES
                {
                    return Err(HostError::failed("Mock model output exceeds 1 MiB"));
                }
                chunks.push(Ok(ModelChunk::ToolStart {
                    id: call.id.clone(),
                    name: call.name.clone(),
                }));
                chunks.push(Ok(ModelChunk::ToolUse(call)));
                ModelStopReason::ToolUse
            } else {
                let prefix = if last.is_some_and(|message| message.role == Role::ToolResult) {
                    "Tool result: "
                } else {
                    "Echo: "
                };
                if text.len() > MAX_MOCK_OUTPUT_BYTES - prefix.len() {
                    return Err(HostError::failed("Mock model output exceeds 1 MiB"));
                }
                chunks.push(Ok(ModelChunk::Text(format!("{prefix}{text}"))));
                chunks.push(Ok(ModelChunk::TextEnd));
                ModelStopReason::EndTurn
            };
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

fn delegated_message_body(text: &str) -> Option<String> {
    if !text.starts_with("<dim-agent-message ") || !text.ends_with("\n</dim-agent-message>") {
        return None;
    }
    let body = text
        .split_once(":\n")?
        .1
        .strip_suffix("\n</dim-agent-message>")?;
    Some(
        body.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn message_text(message: &Message) -> Result<String, HostError> {
    let mut output = String::new();
    let mut first = true;
    let mut append = |text: &str| {
        let separator = usize::from(!first);
        if output.len() + separator > MAX_MOCK_OUTPUT_BYTES
            || text.len() > MAX_MOCK_OUTPUT_BYTES.saturating_sub(output.len() + separator)
        {
            return Err(HostError::failed("Mock model output exceeds 1 MiB"));
        }
        if !first {
            output.push('\n');
        }
        output.push_str(text);
        first = false;
        Ok(())
    };
    match &message.content {
        MessageContent::Text(text) => append(text)?,
        MessageContent::Parts(parts) => {
            for part in parts {
                match part {
                    MessagePart::Text { text, .. } => append(text)?,
                    MessagePart::ToolResult { content, .. } => match content {
                        ToolContent::Text(text) => append(text)?,
                        ToolContent::Blocks(blocks) => {
                            for block in blocks {
                                if let ContentBlock::Text { text } = block {
                                    append(text)?;
                                }
                            }
                        }
                    },
                    _ => {}
                }
            }
        }
    }
    Ok(output)
}

pub struct MockTools;
impl ToolDriver for MockTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                risk: Some("session".into()),
                ..definition(
                    "echo",
                    "Return text without side effects.",
                    serde_json::json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"text":{"type":"string"}}}),
                )
            },
            ToolDefinition {
                risk: Some("session".into()),
                ..definition(
                    "wait",
                    "Wait for milliseconds, then return text. Cancellation waits for the host to finish.",
                    serde_json::json!({"type":"object","required":["millis"],"additionalProperties":false,"properties":{"millis":{"type":"number","minimum":0,"maximum":60000},"text":{"type":"string"}}}),
                )
            },
        ]
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            crate::context_policy::check_cancel(&cancel)?;
            match call.name.as_str() {
                "echo" => {
                    #[derive(serde::Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Input {
                        text: String,
                    }
                    let input: Input =
                        serde_json::from_value(serde_json::Value::Object(call.input))
                            .map_err(|_| HostError::failed("Invalid echo tool input"))?;
                    if input.text.len() > MAX_MOCK_OUTPUT_BYTES {
                        return Err(HostError::failed("Mock tool output exceeds 1 MiB"));
                    }
                    Ok(ToolResult::text(input.text))
                }
                "wait" => {
                    #[derive(serde::Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Input {
                        millis: u64,
                        text: Option<String>,
                    }
                    let input: Input =
                        serde_json::from_value(serde_json::Value::Object(call.input))
                            .map_err(|_| HostError::failed("Invalid wait tool input"))?;
                    if input.millis > 60_000 {
                        return Err(HostError::failed("millis exceeds 60000"));
                    }
                    if input
                        .text
                        .as_ref()
                        .is_some_and(|text| text.len() > MAX_MOCK_OUTPUT_BYTES)
                    {
                        return Err(HostError::failed("Mock tool output exceeds 1 MiB"));
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => Err(HostError::Cancelled("Wait cancelled".into())),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(input.millis)) => Ok(ToolResult::text(input.text.unwrap_or_else(|| "Wait completed".into()))),
                    }
                }
                _ => Ok(ToolResult::error("Unknown mock tool")),
            }
        })
    }
}

pub struct DefaultPrepareModel {
    pub config: Arc<SessionConfig>,
}
impl PrepareModel for DefaultPrepareModel {
    fn prepare(
        &self,
        _: &str,
        _: &str,
        _: &[Arc<Message>],
        available_tools: &[ToolDefinition],
    ) -> Result<ModelSettings, HostError> {
        let mut settings =
            ModelSettings::new(&self.config.model.provider_id, &self.config.model.model_id);
        settings.capability_snapshot = self.config.model.capability_snapshot.clone();
        settings.system = (!self.config.system_prompt.is_empty())
            .then(|| SystemPrompt::Text(self.config.system_prompt.clone()));
        settings.tools = Some(available_tools.to_vec());
        settings.config = crate::context_model::adopted_config(
            settings.capability_snapshot.as_ref(),
            self.config.model.config.as_ref(),
        );
        settings.metadata = self.config.model.metadata.clone();
        Ok(settings)
    }
}
pub struct DefaultPrepareModelPlugin {
    host: Key<PrepareModelHost>,
    config: Key<SessionConfig>,
}
impl DefaultPrepareModelPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names.key()?,
            config: names.key()?,
        })
    }
}
impl DynPlugin for DefaultPrepareModelPlugin {
    fn name(&self) -> &str {
        "PrepareModelHost"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.config.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            ctx.provide_at(
                self.host.clone(),
                Arc::new(PrepareModelHost(Arc::new(DefaultPrepareModel { config }))),
            )?;
            Ok(Effect::Done)
        })
    }
}
