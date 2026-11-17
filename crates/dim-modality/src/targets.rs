use crate::{OcrLookup, Reason, catalog_category, engine::Shared, evaluate};
use dim_agent_capabilities::methods::RpcError;
use dim_agent_types::ModelId;
use rutis_bridge::session::{Error as BridgeError, HostDispatch, Reply, Value, arg};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One operation's target, optionally overriding its configured default.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetRequest {
    pub category: String,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
}

/// The current target or the snapshot's reasons it cannot be used.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TargetReply {
    Resolved { target: ModelId },
    Unavailable { reasons: Vec<Reason> },
}

/// A malformed target query, failed owner read, cancellation or ended generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TargetError {}

impl From<RpcError> for TargetError {
    fn from(error: RpcError) -> Self {
        Self {
            code: error
                .data
                .expect("Modality owner errors carry an application code")
                .app_error,
            message: error.message,
        }
    }
}

/// The process `modalityTargets` data face; native and wire reads use the same owner.
#[derive(Clone)]
pub struct ModalityTargets {
    shared: Arc<Shared>,
}

impl ModalityTargets {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Resolves from current Settings and Provider reads without creating a task
    /// or resolving a credential. An unavailable default never selects another
    /// model. Cancelling or dropping this read releases only this call's work.
    pub async fn resolve(
        &self,
        request: TargetRequest,
        cancel: CancellationToken,
    ) -> Result<TargetReply, TargetError> {
        if self.shared.stop.is_cancelled() {
            return Err(RpcError::reloading().into());
        }
        if cancel.is_cancelled() {
            return Err(RpcError::interrupted().into());
        }
        let category = catalog_category(&request.category)
            .ok_or_else(|| crate::unknown_operation("Unknown modality category"))?;
        let row = category
            .rows
            .iter()
            .find(|row| row.operation == request.operation)
            .ok_or_else(|| crate::unknown_operation("Unknown modality operation"))?;
        let cancel = cancel.child_token();
        let _dropped = cancel.clone().drop_guard();
        let (_, evaluation) = tokio::select! {
            biased;
            _ = self.shared.stop.cancelled() => return Err(RpcError::reloading().into()),
            _ = cancel.cancelled() => return Err(RpcError::interrupted().into()),
            result = evaluate(
                &self.shared.settings,
                &self.shared.selectable,
                &self.shared.selection,
                &self.shared.ctx,
                self.shared.ocr_helper.as_deref(),
                &cancel,
                if category.name == "ocr" { OcrLookup::Read } else { OcrLookup::Skip },
            ) => result?,
        };
        let snapshot = evaluation.operation(category.name, row);
        if let Some(model) = request.model {
            if !snapshot.options.iter().any(|option| {
                option.available
                    && option.model() == (model.provider_id.as_str(), model.model_id.as_str())
            }) {
                return Ok(TargetReply::Unavailable {
                    reasons: vec![evaluation.unavailable_reason(
                        &model,
                        category.name,
                        row.operation,
                        &row.id(category.name),
                    )],
                });
            }
            if snapshot.available {
                return Ok(TargetReply::Resolved { target: model });
            }
        } else if let Some(default) = &snapshot.default
            && !default.available
        {
            return Ok(TargetReply::Unavailable {
                reasons: vec![
                    default
                        .reason
                        .clone()
                        .expect("unavailable defaults have a reason"),
                ],
            });
        } else if snapshot.available {
            let effective = snapshot
                .effective
                .expect("available operations resolve a target");
            return Ok(TargetReply::Resolved {
                target: ModelId {
                    provider_id: effective.provider_id,
                    model_id: effective.model_id,
                },
            });
        }
        Ok(TargetReply::Unavailable {
            reasons: snapshot.reasons,
        })
    }
}

impl HostDispatch for ModalityTargets {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"resolve": "async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "resolve" {
            return Err(BridgeError::Value(format!(
                "unknown modalityTargets method {method}"
            )));
        }
        let invalid = |message: String| {
            rutis_bridge::exports::failure("modalityTargets", "invalid_params", message, None)
        };
        let args = args.list().map_err(|error| invalid(error.to_string()))?;
        let [request] = args.as_slice() else {
            return Err(invalid("resolve expects one request object".into()));
        };
        let request: TargetRequest = serde_json::from_value(
            request
                .clone()
                .json()
                .map_err(|error| invalid(error.to_string()))?,
        )
        .map_err(|error| invalid(error.to_string()))?;
        let targets = self.clone();
        Ok(Value::cancellable_future(async move {
            match targets.resolve(request, CancellationToken::new()).await {
                Ok(reply) => arg(&reply),
                Err(error) => Err(rutis_bridge::exports::failure(
                    "modalityTargets",
                    &error.code,
                    error.message,
                    None,
                )),
            }
        }))
    }
}
