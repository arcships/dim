//! The auxiliary-model operation: one binding and one run, shared by every
//! owner of model selection.
//!
//! The owner resolves the model, the configuration it would send for a
//! Session request (its adopted output allowance included), then calls
//! [`complete`].
//! Whoever owns model selection exports the operation with [`provide`], so a
//! profile that serves a model offers the same `auxiliaryModel` name to
//! Session parts ([AuxiliaryModel §4.3](../../../docs/design/appserver/auxiliary-model.md#43-named-binding)).
use crate::host::{
    AuxiliaryError, AuxiliaryErrorCode, AuxiliaryModel, AuxiliaryReply, AuxiliaryRequest,
    CompleteAuxiliary, HostError, ModelChunk, ModelConnection, ModelHost, ModelStopReason,
    ModelStream, ResolveModelConnection,
};
use crate::images::takes_image_format;
use crate::model_provider::invoke_with_recovery;
use dim_agent_types::{
    ContentBlock, MessageContent, MessagePart, ModelCapabilitySnapshot, ModelConfig, ModelId,
    ModelProtocol, ModelRequest, SystemPrompt, ToolContent,
};
use futures_util::StreamExt;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// What the model's owner resolves before the shared run.
pub struct AuxiliaryTarget {
    /// The model the caller named; the owner never substitutes another.
    pub model: ModelId,
    /// The configuration the owner would send for a Session request, with the
    /// model's adopted output allowance in `max_tokens`.
    pub config: ModelConfig,
    pub capability_snapshot: Option<ModelCapabilitySnapshot>,
}

/// Checks the caller's request before an owner admits it. Each owner calls
/// this first, so an invalid request is neither admitted nor logged;
/// [`complete`] does not repeat the check.
pub fn check(request: &AuxiliaryRequest) -> Result<(), AuxiliaryError> {
    if request.messages.is_empty() {
        return Err(invalid("messages must not be empty"));
    }
    if request.max_output_tokens == Some(0) {
        return Err(invalid("maxOutputTokens must be positive"));
    }
    Ok(())
}

/// Runs one auxiliary call: one non-streaming request with no tools and no
/// Timeline, under the shared retry and credential-recovery policy. The
/// connection comes from the owner's resolver; credentials never leave it.
/// The owner checked the request with [`check`] before this run.
pub async fn complete(
    host: &ModelHost,
    connection: &dyn ResolveModelConnection,
    target: AuxiliaryTarget,
    request: AuxiliaryRequest,
    cancel: &CancellationToken,
) -> Result<AuxiliaryReply, AuxiliaryError> {
    let mut config = target.config;
    let allowance = config.max_tokens.ok_or_else(|| {
        invalid("The model output allowance is not adopted; select the model again")
    })?;
    config.max_tokens = Some(
        request
            .max_output_tokens
            .map_or(allowance, |max| f64::from(max).min(allowance)),
    );
    let mut model_request = ModelRequest::new(
        target.model.provider_id,
        target.model.model_id,
        request.messages,
    );
    model_request.system = request
        .system
        .map(|text| Arc::new(SystemPrompt::Text(text)));
    model_request.capability_snapshot = target.capability_snapshot;
    model_request.config = Some(config);
    let model_request = Arc::new(model_request);
    let resolved = connection
        .resolve(model_request.clone(), cancel.clone())
        .await?;
    require_sendable_images(&model_request, &resolved.connection)?;
    invoke_with_recovery(
        host,
        &model_request,
        None,
        resolved,
        cancel,
        &mut (),
        |stream, _, _, _| Box::pin(collect(stream)),
    )
    .await
    .map_err(AuxiliaryError::from)
}

/// A standalone call has no conversation a content reference could resolve
/// against, so an image the destination does not take as pixels fails the
/// call instead of becoming a reference the model cannot read.
fn require_sendable_images(
    request: &ModelRequest,
    connection: &ModelConnection,
) -> Result<(), AuxiliaryError> {
    let (protocol, preset) = match connection {
        ModelConnection::Http(connection) => {
            (Some(connection.protocol), connection.preset.as_deref())
        }
        ModelConnection::Bedrock(_) => (Some(ModelProtocol::Bedrock), None),
        ModelConnection::Local => (None, None),
    };
    for message in &request.messages {
        let MessageContent::Parts(parts) = &message.content else {
            continue;
        };
        for part in parts {
            match part {
                MessagePart::Image { source } => {
                    require_sendable_image(protocol, preset, &source.media_type)?
                }
                MessagePart::ToolResult {
                    content: ToolContent::Blocks(blocks),
                    ..
                } => {
                    for block in blocks {
                        if let ContentBlock::Image { source } = block {
                            require_sendable_image(protocol, preset, &source.media_type)?;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Fails when the destination does not take the image's format as pixels. A
/// standalone call has no conversation a content reference could resolve
/// against, so it refuses the image instead of sending pixels the target does
/// not take; an owner that reuses an answer calls this too, so a reused answer
/// follows the format rule of the route that serves the target now. `preset` is
/// the vendor package the route's compatible protocol builds, if any.
pub fn require_sendable_image(
    protocol: Option<ModelProtocol>,
    preset: Option<&str>,
    media_type: &str,
) -> Result<(), AuxiliaryError> {
    if takes_image_format(protocol, preset, media_type) {
        return Ok(());
    }
    Err(AuxiliaryError {
        code: AuxiliaryErrorCode::InvalidRequest,
        message: format!("The image model does not accept {media_type}"),
    })
}

/// Records one finished call under `auxiliary.complete`: provider, model,
/// outcome, duration and token usage, never request or reply content.
pub fn record(model: &ModelId, started: Instant, result: Result<&AuxiliaryReply, &AuxiliaryError>) {
    let outcome = match result {
        Ok(_) => "completed",
        Err(error) => match error.code {
            AuxiliaryErrorCode::ModelUnavailable => "model_unavailable",
            AuxiliaryErrorCode::Cancelled => "cancelled",
            AuxiliaryErrorCode::InvalidRequest | AuxiliaryErrorCode::Failed => "failed",
        },
    };
    let usage = result.ok().and_then(|reply| reply.usage.as_ref());
    tracing::info!(
        target: "auxiliary.complete",
        provider_id = %model.provider_id,
        model_id = %model.model_id,
        outcome,
        duration_ms = started.elapsed().as_millis() as u64,
        prompt_tokens = usage.map(|usage| usage.prompt_tokens),
        completion_tokens = usage.map(|usage| usage.completion_tokens),
    );
}

/// Provides one owner's operation as the Rust port and under the shared
/// `auxiliaryModel` name Session parts inject.
pub fn provide(ctx: &Ctx, operation: Arc<dyn CompleteAuxiliary>) -> rutis::Result<()> {
    let binding = Arc::new(AuxiliaryModel(operation));
    ctx.provide_as::<AuxiliaryModel>(binding.clone())?;
    ctx.provide_at(
        rutis_bridge::session::host_key("auxiliaryModel"),
        binding as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteRequest {
    model: ModelInput,
    request: AuxiliaryRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelInput {
    provider_id: String,
    model_id: String,
}

impl HostDispatch for AuxiliaryModel {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"complete": "async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "complete" {
            return Err(Error::value(format!(
                "unknown auxiliaryModel method {method}"
            )));
        }
        let mut args = args
            .list()
            .map_err(|_| failure(invalid("Invalid auxiliaryModel arguments")))?
            .into_iter();
        let request = args
            .next()
            .ok_or_else(|| failure(invalid("Missing auxiliaryModel request")))?;
        let CompleteRequest { model, request } = request
            .json()
            .ok()
            .and_then(|request| serde_json::from_value(request).ok())
            .ok_or_else(|| failure(invalid("Invalid auxiliaryModel request")))?;
        let model = ModelId {
            provider_id: model.provider_id,
            model_id: model.model_id,
        };
        let cancelled = match args.next() {
            None | Some(Value::Undefined) => None,
            Some(Value::Reference(reference)) if reference.is_function() => Some(reference),
            Some(_) => return Err(failure(invalid("Invalid auxiliaryModel cancellation"))),
        };
        if args.next().is_some() {
            return Err(failure(invalid("Too many auxiliaryModel arguments")));
        }
        let operation = self.0.clone();
        Ok(Value::future(async move {
            let cancel = CancellationToken::new();
            let call = operation.complete(model, request, cancel.clone());
            tokio::pin!(call);
            let reply = if let Some(cancelled) = cancelled {
                tokio::select! {
                    result = &mut call => result,
                    _ = async {
                        let returned = cancelled.call_async(Value::List(vec![])).await?;
                        rutis_bridge::session::settle(returned).await
                    } => {
                        cancel.cancel();
                        call.await
                    }
                }
            } else {
                call.await
            }
            .map_err(failure)?;
            rutis_bridge::session::arg(&reply)
        }))
    }
}

fn failure(error: AuxiliaryError) -> Error {
    let code = match error.code {
        AuxiliaryErrorCode::InvalidRequest => "invalid_request",
        AuxiliaryErrorCode::ModelUnavailable => "model_unavailable",
        AuxiliaryErrorCode::Failed => "failed",
        AuxiliaryErrorCode::Cancelled => "cancelled",
    };
    rutis_bridge::exports::failure("AuxiliaryError", code, error.message, None)
}

fn invalid(message: &str) -> AuxiliaryError {
    AuxiliaryError {
        code: AuxiliaryErrorCode::InvalidRequest,
        message: message.into(),
    }
}

async fn collect(mut stream: ModelStream) -> Result<AuxiliaryReply, HostError> {
    let (mut text, mut usage, mut stop_reason) = (String::new(), None, None);
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ModelChunk::Text(delta) => text.push_str(&delta),
            ModelChunk::Usage(value) => usage = Some(value),
            ModelChunk::MessageStop { reason, .. } => stop_reason = Some(reason),
            _ => {}
        }
    }
    let stop_reason =
        stop_reason.ok_or_else(|| HostError::failed("Model stream ended without a stop"))?;
    if stop_reason == ModelStopReason::ToolUse {
        return Err(HostError::failed(
            "Model stopped for a tool it was not offered",
        ));
    }
    Ok(AuxiliaryReply {
        text,
        stop_reason,
        usage,
    })
}
