use dim_agent_capabilities::host::{HostError, ModelProtocol};
use dim_agent_types::{ContentMetadata, MessagePart};
use serde_json::Value;

/// Interpret only native continuation fields; unrelated diagnostics stay in aimux.
pub(super) fn metadata(
    protocol: ModelProtocol,
    value: Option<&Value>,
) -> Result<Option<ContentMetadata>, HostError> {
    let namespace = match protocol {
        ModelProtocol::Bedrock => "bedrock",
        ModelProtocol::Anthropic => "anthropic",
        ModelProtocol::Gemini => "google",
        ModelProtocol::OpenaiResponses => "openai",
        ModelProtocol::XaiResponses => "xai",
        _ => return Ok(None),
    };
    let Some(fields) = value.and_then(|value| value.get(namespace)) else {
        return Ok(None);
    };
    let string = |key: &str| -> Result<Option<String>, HostError> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            _ => Err(HostError::failed(format!(
                "Invalid native continuation field: {namespace}.{key}"
            ))),
        }
    };
    // A continuation field carries a value only when it is nonempty: an empty
    // signature, redacted payload or encrypted payload is the same as absent.
    let continuation = |key: &str| -> Result<Option<String>, HostError> {
        Ok(string(key)?.filter(|value| !value.is_empty()))
    };
    Ok(match protocol {
        ModelProtocol::Bedrock => match (string("signature")?, string("redactedData")?) {
            (Some(signature), None) => Some(ContentMetadata::Bedrock {
                signature: Some(signature),
            }),
            (None, Some(data)) => Some(ContentMetadata::BedrockRedacted { data }),
            (None, None) => Some(ContentMetadata::Bedrock { signature: None }),
            _ => {
                return Err(HostError::failed(
                    "Bedrock thinking cannot be both signed and redacted",
                ));
            }
        },
        ModelProtocol::Anthropic => {
            match (continuation("signature")?, continuation("redactedData")?) {
                (Some(signature), None) => Some(ContentMetadata::Anthropic { signature }),
                (None, Some(data)) => Some(ContentMetadata::AnthropicRedacted { data }),
                (None, None) => None,
                _ => {
                    return Err(HostError::failed(
                        "Thinking cannot be both signed and redacted",
                    ));
                }
            }
        }
        ModelProtocol::Gemini => string("thoughtSignature")?
            .map(|thought_signature| ContentMetadata::Gemini { thought_signature }),
        ModelProtocol::XaiResponses => {
            let encrypted_content = continuation("reasoningEncryptedContent")?;
            match string("itemId")? {
                Some(item_id) => Some(ContentMetadata::XaiResponses {
                    item_id,
                    encrypted_content,
                }),
                None if encrypted_content.is_some() => {
                    return Err(HostError::failed(
                        "Native continuation is missing item identity",
                    ));
                }
                None => None,
            }
        }
        ModelProtocol::OpenaiResponses => {
            let encrypted_content = continuation("reasoningEncryptedContent")?;
            let phase = string("phase")?;
            let summary_index = match fields.get("summaryIndex") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| HostError::failed("Invalid native summary index"))?,
                ),
            };
            match string("itemId")? {
                Some(item_id) => Some(ContentMetadata::OpenaiResponses {
                    item_id,
                    encrypted_content,
                    summary_index,
                    phase,
                }),
                None if encrypted_content.is_some()
                    || summary_index.is_some()
                    || phase.is_some() =>
                {
                    return Err(HostError::failed(
                        "Native continuation is missing item identity",
                    ));
                }
                None => None,
            }
        }
        _ => unreachable!(),
    })
}

pub(super) fn update(part: &mut MessagePart, incoming: Option<ContentMetadata>) {
    let target = match part {
        MessagePart::Text {
            provider_metadata, ..
        }
        | MessagePart::Thinking {
            provider_metadata, ..
        }
        | MessagePart::ToolUse {
            provider_metadata, ..
        } => provider_metadata,
        _ => unreachable!(),
    };
    if let Some(mut incoming) = incoming {
        if let (
            Some(ContentMetadata::OpenaiResponses {
                item_id: old_id,
                encrypted_content: old_encrypted,
                summary_index: old_index,
                phase: old_phase,
            }),
            ContentMetadata::OpenaiResponses {
                item_id,
                encrypted_content,
                summary_index,
                phase,
            },
        ) = (target.as_ref(), &mut incoming)
            && old_id == item_id
        {
            *encrypted_content = encrypted_content.take().or_else(|| old_encrypted.clone());
            *summary_index = summary_index.or(*old_index);
            *phase = phase.take().or_else(|| old_phase.clone());
        }
        *target = Some(incoming);
    }
}
