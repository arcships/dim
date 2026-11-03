//! The auxiliary-model operation: one binding and one run, shared by every
//! owner of model selection.
//!
//! The owner resolves the model, the configuration it would send for a
//! Session request and its declared output limit, then calls [`complete`].
//! Whoever owns model selection exports the operation with [`provide`], so a
//! profile that serves a model offers the same `auxiliaryModel` name to
//! Session parts ([AuxiliaryModel §4.3](../../../docs/design/appserver/auxiliary-model.md#43-named-binding)).
use crate::host::{
    AuxiliaryError, AuxiliaryErrorCode, AuxiliaryModelHost, AuxiliaryReply, AuxiliaryRequest,
    CompleteAuxiliary, HostError, ModelChunk, ModelHost, ModelStopReason, ModelStream,
    ResolveModelConnection,
};
use crate::model_provider::invoke_with_recovery;
use dim_agent_types::{ModelCapabilitySnapshot, ModelConfig, ModelId, ModelRequest, SystemPrompt};
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
    /// The configuration the owner would send for a Session request.
    pub config: ModelConfig,
    pub capability_snapshot: Option<ModelCapabilitySnapshot>,
    /// The model's declared output limit.
    pub max_output_tokens: u64,
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
    let limit = target.max_output_tokens;
    let mut config = target.config;
    config.max_tokens = Some(
        request
            .max_output_tokens
            .map_or(limit, |max| u64::from(max).min(limit)) as f64,
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
    invoke_with_recovery(
        host,
        &model_request,
        resolved,
        cancel,
        &mut (),
        |stream, _, _, _| Box::pin(collect(stream)),
    )
    .await
    .map_err(AuxiliaryError::from)
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
    ctx.provide(AuxiliaryModelHost(operation.clone()))?;
    ctx.provide_at(
        rutis_bridge::session::host_key("auxiliaryModel"),
        Arc::new(Binding(operation)) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

/// The language-neutral form of one owner's operation: the call shape, its
/// optional cancellation function, and the error `{code, message}`.
struct Binding(Arc<dyn CompleteAuxiliary>);

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

impl HostDispatch for Binding {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"complete": "async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "complete" {
            return Err(Error::Value(format!(
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
