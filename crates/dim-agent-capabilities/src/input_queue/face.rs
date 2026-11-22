use super::{CheckSubmission, SubmitCheckReply, SubmitCheckRequest, SubmitChecks};
use crate::{
    host::{HostError, HostFuture},
    port_faces::{invalid, unknown_method},
};
use rutis::host::CtxHost;
use rutis_bridge::session::{HostDispatch, Reference, Reply, Value, arg, decode_value, settle};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct ForeignCheck {
    callback: Reference,
    closing: CancellationToken,
}

impl CheckSubmission for ForeignCheck {
    fn check(
        &self,
        request: SubmitCheckRequest,
        cancel: CancellationToken,
    ) -> HostFuture<'_, SubmitCheckReply> {
        Box::pin(async move {
            let stop = cancel.child_token();
            let _finish = stop.clone().drop_guard();
            let cancelled = Value::callback(move |_| {
                let stop = stop.clone();
                Ok(Value::future(async move {
                    stop.cancelled().await;
                    Ok(Value::Undefined)
                }))
            });
            let reply = async {
                let result = self
                    .callback
                    .call_async(Value::List(vec![arg(&request)?, cancelled]))
                    .await?;
                decode_value(settle(result).await?)
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(HostError::Cancelled("submit check interrupted".into())),
                _ = self.closing.cancelled() => Err(HostError::failed("submit check unloaded")),
                result = reply => result.map_err(|error| HostError::failed(error.to_string())),
            }
        })
    }
}

impl HostDispatch for SubmitChecks {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"register":"async","check":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "register" => {
                let [name, Value::Reference(callback)] = <[_; 2]>::try_from(args.list()?)
                    .map_err(|_| invalid("submitChecks", "register expects a name and a check"))?
                else {
                    return Err(invalid("submitChecks", "expected a check function"));
                };
                if !callback.is_function() {
                    return Err(invalid("submitChecks", "expected a check function"));
                }
                let name: String = decode_value(name)?;
                let row = rutis_bridge::session::caller_row()
                    .ok_or_else(|| invalid("submitChecks", "register requires a calling row"))?;
                let registered = self
                    .register(
                        row.ctx(),
                        &name,
                        Arc::new(ForeignCheck {
                            callback,
                            closing: row.ctx().cancellation_token(),
                        }),
                    )
                    .map(|()| Value::Undefined)
                    .map_err(|error| {
                        rutis_bridge::exports::failure(
                            "SubmitCheckError",
                            "failed",
                            error.to_string(),
                            None,
                        )
                    });
                Ok(Value::future(async move { registered }))
            }
            "check" => {
                let [request, Value::Reference(cancelled)] = <[_; 2]>::try_from(args.list()?)
                    .map_err(|_| {
                        invalid(
                            "submitChecks",
                            "check expects a request and cancellation function",
                        )
                    })?
                else {
                    return Err(invalid("submitChecks", "expected a cancellation function"));
                };
                if !cancelled.is_function() {
                    return Err(invalid("submitChecks", "expected a cancellation function"));
                }
                let request: SubmitCheckRequest = decode_value(request)?;
                let owner = self.clone();
                let stop = rutis_bridge::session::caller_row()
                    .map(|row| row.ctx().cancellation_token().child_token())
                    .unwrap_or_default();
                Ok(Value::cancellable_future(async move {
                    let _finish = stop.clone().drop_guard();
                    let run = owner.check(&request, &stop);
                    tokio::pin!(run);
                    let cancellation =
                        async { settle(cancelled.call_async(Value::List(vec![])).await?).await };
                    let result = tokio::select! {
                        result = &mut run => result,
                        _ = cancellation => {
                            stop.cancel();
                            run.await
                        }
                    }
                    .map_err(|_| {
                        rutis_bridge::exports::failure(
                            "SubmitCheckError",
                            "cancelled",
                            "submit check interrupted".into(),
                            None,
                        )
                    })?;
                    arg(&result)
                }))
            }
            _ => Err(unknown_method("submitChecks", method)),
        }
    }
}
