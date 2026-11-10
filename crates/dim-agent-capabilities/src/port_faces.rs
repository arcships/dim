//! The data faces of host ports: the request, reply and error another
//! language calls ([service faces §1](../../../docs/design/service-faces.md#1-规则)).
//!
//! A port keeps its Rust trait for Rust callers; its face is the same
//! operation as data, provided beside the Rust value under the port's own
//! name, so a part of any language calls what a Rust part reads. The owner
//! is the port's implementation: a face delegates to it and adds no rule of
//! its own.
use crate::host::{
    HostError, HostErrorKind, ModelSelectionHost, ResolveModelSelection, SelectableModels,
    SelectableModelsHost, optional_non_null,
};
use dim_agent_runtime::names::name_of;
use dim_agent_types::{ModalityModelSnapshot, ModelCapabilitySnapshot, ModelId, ModelSelection};
use rutis::Ctx;
use rutis::host::{CtxHost, Key};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reference, Reply, Value as BridgeValue, host_key,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One owner failure as its caller of another language reads it: the port
/// that failed, the code and the message.
pub(crate) fn port_failure(name: &str, code: &str, message: impl Into<String>) -> BridgeError {
    rutis_bridge::exports::failure(name, code, message.into(), None)
}

/// A caller's argument a port refuses: `invalid_request`, naming the port.
pub(crate) fn invalid(name: &str, message: impl std::fmt::Display) -> BridgeError {
    port_failure(name, "invalid_request", format!("{name}: {message}"))
}

/// A method the port does not serve.
pub(crate) fn unknown_method(name: &str, method: &str) -> BridgeError {
    BridgeError::Value(format!("{name} has no method {method}"))
}

/// One host failure as its caller reads it: the failure's kind is the code,
/// and an owner that closed is as unusable as a cancelled call.
pub(crate) fn host_error(error: HostError) -> BridgeError {
    let code = match &error {
        HostError::Cancelled(_) => "cancelled",
        HostError::Failed { kind, .. } => match kind {
            HostErrorKind::InvalidRequest => "invalid_request",
            HostErrorKind::Unsupported => "unsupported",
            HostErrorKind::Transport => "transport",
            HostErrorKind::OwnerClosed => "cancelled",
            HostErrorKind::Http { .. } | HostErrorKind::Other => "failed",
        },
    };
    rutis_bridge::exports::failure("HostError", code, error.to_string(), None)
}

/// The request of a port method and the cancellation function after it.
pub(crate) fn request_args(
    name: &str,
    args: BridgeValue,
) -> Result<(BridgeValue, Option<Reference>), BridgeError> {
    let mut args = argument_list(name, args)?;
    if args.is_empty() || args.len() > 2 {
        return Err(invalid(
            name,
            "expected a request and the optional cancellation",
        ));
    }
    args.resize(2, BridgeValue::Undefined);
    let mut args = args.into_iter();
    let request = args.next().expect("two arguments");
    let cancelled = cancellation(name, args.next().expect("two arguments"))?;
    Ok((request, cancelled))
}

/// Checks that a port method with no arguments receives none.
pub(crate) fn no_args(name: &str, method: &str, args: BridgeValue) -> Result<(), BridgeError> {
    if argument_list(name, args)?.is_empty() {
        return Ok(());
    }
    Err(invalid(name, format!("{method} takes no arguments")))
}

/// The cancellation function of a port method that has no request.
pub(crate) fn cancel_arg(name: &str, args: BridgeValue) -> Result<Option<Reference>, BridgeError> {
    let mut args = argument_list(name, args)?;
    if args.len() > 1 {
        return Err(invalid(name, "expected only the cancellation"));
    }
    cancellation(name, args.pop().unwrap_or(BridgeValue::Undefined))
}

fn argument_list(name: &str, args: BridgeValue) -> Result<Vec<BridgeValue>, BridgeError> {
    match args {
        BridgeValue::Undefined => Ok(Vec::new()),
        args => args
            .list()
            .map_err(|_| invalid(name, "the arguments are not a list")),
    }
}

fn cancellation(name: &str, arg: BridgeValue) -> Result<Option<Reference>, BridgeError> {
    match arg {
        BridgeValue::Undefined => Ok(None),
        BridgeValue::Reference(reference) if reference.is_function() => Ok(Some(reference)),
        _ => Err(invalid(name, "the cancellation must be a function")),
    }
}

/// Runs one port call on `cancel`, which the caller's cancellation function
/// cancels when it passes one. The call's own result stands once it observes
/// the cancellation: cancellation is a call, not a request field.
pub(crate) async fn cancelled_by<'a, T, E>(
    cancel: &CancellationToken,
    cancelled: Option<Reference>,
    call: std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'a>>,
) -> Result<T, E> {
    let Some(cancelled) = cancelled else {
        return call.await;
    };
    tokio::pin!(call);
    tokio::select! {
        result = &mut call => result,
        _ = async {
            let returned = cancelled.call_async(BridgeValue::List(vec![])).await?;
            rutis_bridge::session::settle(returned).await
        } => {
            cancel.cancel();
            call.await
        }
    }
}

/// Provides one owner's [`ModelSelectionHost`] as the Rust value parts read
/// and as the `modelSelectionHost` face other languages call.
pub fn provide_model_selection(ctx: &Ctx, port: &ModelSelectionHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<ModelSelectionHost>::of(),
        Arc::new(ModelSelectionHost(port.0.clone())),
    )?;
    ctx.provide_at(
        host_key(&name_of::<ModelSelectionHost>()),
        Arc::new(ModelSelectionFace(port.0.clone())) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

/// The `resolve` request: the model the caller names, if any, and the
/// directory a project's configuration is read from.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelectionRequest {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub explicit: Option<ModelId>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
}

const MODEL_SELECTION: &str = "modelSelectionHost";

struct ModelSelectionFace(Arc<dyn ResolveModelSelection>);

impl HostDispatch for ModelSelectionFace {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"resolve": "async"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "resolve" {
            return Err(unknown_method(MODEL_SELECTION, method));
        }
        let (request, cancelled) = request_args(MODEL_SELECTION, args)?;
        let request: ModelSelectionRequest = serde_json::from_value(
            request
                .json()
                .map_err(|_| invalid(MODEL_SELECTION, "the request is not data"))?,
        )
        .map_err(|error| invalid(MODEL_SELECTION, error))?;
        let operation = self.0.clone();
        Ok(BridgeValue::future(async move {
            let cancel = CancellationToken::new();
            let selection: ModelSelection = cancelled_by(
                &cancel,
                cancelled,
                operation.resolve(
                    request.explicit,
                    request.cwd.map(std::path::PathBuf::from),
                    cancel.clone(),
                ),
            )
            .await
            .map_err(host_error)?;
            rutis_bridge::session::arg(&selection)
        }))
    }
}

/// Provides one owner's [`SelectableModelsHost`] as the Rust value parts read
/// and as the `selectableModelsHost` face other languages call.
pub fn provide_selectable_models(ctx: &Ctx, port: &SelectableModelsHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<SelectableModelsHost>::of(),
        Arc::new(SelectableModelsHost(port.0.clone())),
    )?;
    ctx.provide_at(
        host_key(&name_of::<SelectableModelsHost>()),
        Arc::new(SelectableModelsFace(port.0.clone())) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

/// One `selectableModelsHost` reply: the models the owner would serve.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectableModelsReply {
    pub models: Vec<ModelCapabilitySnapshot>,
}

/// One `selectableModelsHost.selectableForModality` reply: the models the
/// owner would serve for a non-conversational use.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalityModelsReply {
    pub models: Vec<ModalityModelSnapshot>,
}

const SELECTABLE_MODELS: &str = "selectableModelsHost";

struct SelectableModelsFace(Arc<dyn SelectableModels>);

impl HostDispatch for SelectableModelsFace {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"selectableModels": "async", "selectableForModality": "async"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        let cancelled = cancel_arg(SELECTABLE_MODELS, args)?;
        let operation = self.0.clone();
        let modality = match method {
            "selectableModels" => false,
            "selectableForModality" => true,
            _ => return Err(unknown_method(SELECTABLE_MODELS, method)),
        };
        Ok(BridgeValue::future(async move {
            let cancel = CancellationToken::new();
            if modality {
                let models = cancelled_by(
                    &cancel,
                    cancelled,
                    operation.selectable_for_modality(cancel.clone()),
                )
                .await
                .map_err(host_error)?;
                rutis_bridge::session::arg(&ModalityModelsReply { models })
            } else {
                let models = cancelled_by(&cancel, cancelled, operation.selectable(cancel.clone()))
                    .await
                    .map_err(host_error)?;
                rutis_bridge::session::arg(&SelectableModelsReply { models })
            }
        }))
    }
}
