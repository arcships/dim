use super::{ToolExecutionContext, ToolExecutor};
use crate::port_faces::{invalid, request, unknown_method};
use dim_agent_types::ToolCall;
use rutis::host::CtxHost;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, arg, settle};
use serde::Deserialize;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const NAME: &str = "toolExecutor";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Open {
    parent_call_id: String,
    context: ToolExecutionContext,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Invoke {
    gate_id: String,
    call: ToolCall,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Close {
    gate_id: String,
}

pub(super) struct Face(pub(super) Arc<ToolExecutor>);

impl HostDispatch for Face {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"open":"sync","invoke":"async","close":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "open" => {
                let args = args.list()?;
                let [data, Value::Reference(cancelled)] = args.as_slice() else {
                    return Err(invalid(
                        NAME,
                        "open requires its request and cancellation function",
                    ));
                };
                if !cancelled.is_function() {
                    return Err(invalid(NAME, "cancellation must be a function"));
                }
                let Open {
                    parent_call_id,
                    context,
                } = serde_json::from_value(data.clone().json()?)
                    .map_err(|error| invalid(NAME, error))?;
                let (id, gate) = self
                    .0
                    .open_gate(parent_call_id, context, CancellationToken::child_token)
                    .map_err(|error| Error::value(error.to_string()))?;
                let cancel = gate.cancel.clone();
                let caller = rutis_bridge::session::caller_row()
                    .map(|row| row.ctx().cancellation_token())
                    .unwrap_or_default();
                let cancelled = cancelled.clone();
                gate.tasks.spawn(async move {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = caller.cancelled() => {},
                        _ = async {
                            let result = cancelled.call_async(Value::List(vec![])).await?;
                            settle(result).await
                        } => {}
                    }
                    cancel.cancel();
                });
                arg(&id)
            }
            "invoke" => {
                let Invoke { gate_id, call } = request(NAME, args)?;
                let owner = self.0.clone();
                Ok(Value::future(async move {
                    arg(&ToolExecutor::invoke(&owner, &gate_id, call)
                        .await
                        .map_err(|error| Error::value(error.to_string()))?)
                }))
            }
            "close" => {
                let Close { gate_id } = request(NAME, args)?;
                let owner = self.0.clone();
                Ok(Value::future(async move {
                    arg(&owner
                        .close(&gate_id)
                        .await
                        .map_err(|error| Error::value(error.to_string()))?)
                }))
            }
            _ => Err(unknown_method(NAME, method)),
        }
    }
}
