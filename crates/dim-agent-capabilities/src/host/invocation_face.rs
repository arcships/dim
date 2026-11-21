use super::*;
use crate::model_provider::{RetryObserver, RetryWaiting};
use futures_util::{StreamExt, future::BoxFuture};
use rutis_bridge::session::{Error, Reference, Value, arg, settle};
use serde::Deserialize;
use tokio::sync::Mutex;

fn invalid(message: impl std::fmt::Display) -> Error {
    crate::port_faces::invalid("modelHost", message)
}

fn function(value: &Value) -> Result<Reference, Error> {
    match value {
        Value::Reference(reference) if reference.is_function() => Ok(reference.clone()),
        _ => Err(invalid("expected a callback")),
    }
}

async fn call(function: &Reference, args: Vec<Value>) -> Reply {
    settle(function.call_async(Value::List(args)).await?).await
}

struct Consumer {
    consume: Reference,
    prepare: Option<Reference>,
    before_attempt: Option<Reference>,
    waiting: Option<Reference>,
    exception: Option<Error>,
    cancel: CancellationToken,
}

impl Consumer {
    async fn callback(
        &mut self,
        function: &Reference,
        args: Vec<Value>,
    ) -> Result<Value, HostError> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(HostError::Cancelled("Model request cancelled".into())),
            result = call(function, args) => result.map_err(|error| self.exception(error)),
        }
    }

    fn exception(&mut self, error: Error) -> HostError {
        let message = error.to_string();
        self.exception = Some(error);
        HostError::failed(message)
    }
}

impl RetryObserver for Consumer {
    async fn prepare(
        &mut self,
        mut request: Arc<ModelRequest>,
        protocol: Option<ModelProtocol>,
    ) -> Result<Arc<ModelRequest>, HostError> {
        if let Some(hook) = self.prepare.clone() {
            let reply = self
                .callback(&hook, vec![Value::Data(json!(protocol))])
                .await?;
            let config: Option<ModelConfig> =
                serde_json::from_value(reply.json().map_err(|e| self.exception(e))?)
                    .map_err(|e| self.exception(invalid(e)))?;
            if let Some(config) = config
                && request.config.as_ref() != Some(&config)
            {
                Arc::make_mut(&mut request).config = Some(config);
            }
        }
        Ok(request)
    }

    async fn before_attempt(&mut self) -> Result<(), HostError> {
        if let Some(hook) = self.before_attempt.clone() {
            let value = self
                .callback(&hook, vec![])
                .await?
                .json()
                .map_err(|e| self.exception(e))?;
            if let Some(reason) = value.get("rejected").and_then(|v| v.as_str()) {
                return Err(HostError::failed(reason));
            }
            if value != "admitted" {
                return Err(self.exception(invalid("expected admitted or a rejection")));
            }
        }
        Ok(())
    }

    async fn waiting(&mut self, retry: Option<RetryWaiting<'_>>) -> Result<(), HostError> {
        if let Some(hook) = self.waiting.clone() {
            self.callback(&hook, vec![Value::Data(json!(retry))])
                .await?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum Outcome {
    Returned {
        value: serde_json::Value,
        replay_safe: bool,
    },
    Failed {
        error: HostError,
        replay_safe: bool,
    },
}

fn consume<'a>(
    stream: ModelStream,
    state: &'a mut Consumer,
    attempt: CancellationToken,
    replay_safe: &'a mut bool,
) -> BoxFuture<'a, Result<serde_json::Value, HostError>> {
    Box::pin(async move {
        let reader = Arc::new(Mutex::new(Some(stream)));
        let active = attempt.child_token();
        let _finish = active.clone().drop_guard();
        let next = {
            let reader = Arc::downgrade(&reader);
            let active = active.clone();
            Value::callback(move |_| {
                let reader = reader
                    .upgrade()
                    .ok_or_else(|| invalid("model reader expired"))?;
                let active = active.clone();
                Ok(Value::future(async move {
                    tokio::select! {
                        biased;
                        _ = active.cancelled() => Err(invalid("model reader expired")),
                        result = async {
                            let mut reader = reader.lock().await;
                            let stream = reader.as_mut().ok_or_else(|| invalid("model reader expired"))?;
                            Ok(Value::Data(match stream.next().await {
                                Some(Ok(chunk)) => json!({"kind":"chunk", "chunk":chunk}),
                                Some(Err(error)) => json!({"kind":"failed", "error":error}),
                                None => json!({"kind":"end"}),
                            }))
                        } => result,
                    }
                }))
            })
        };
        let cancelled = {
            let active = active.clone();
            Value::callback(move |_| {
                let active = active.clone();
                Ok(Value::future(async move {
                    active.cancelled().await;
                    Ok(Value::Undefined)
                }))
            })
        };
        *replay_safe = false;
        let reply = state
            .callback(&state.consume.clone(), vec![next, cancelled])
            .await;
        active.cancel();
        reader.lock().await.take();
        let outcome = serde_json::from_value(reply?.json().map_err(|e| state.exception(e))?)
            .map_err(|e| state.exception(invalid(e)))?;
        match outcome {
            Outcome::Returned {
                value,
                replay_safe: safe,
            } => {
                *replay_safe = safe;
                Ok(value)
            }
            Outcome::Failed {
                error,
                replay_safe: safe,
            } => {
                *replay_safe = safe;
                Err(error)
            }
        }
    })
}

impl HostDispatch for ModelHost {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"invoke":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "invoke" {
            return Err(crate::port_faces::unknown_method("modelHost", method));
        }
        let args = args.list()?;
        let [request, mode, consumer, cancelled, rest @ ..] = args.as_slice() else {
            return Err(invalid(
                "expected request, mode, consume, cancelled and optional hooks",
            ));
        };
        let request = Arc::new(serde_json::from_value(request.clone().json()?).map_err(invalid)?);
        let mode = serde_json::from_value(mode.clone().json()?).map_err(invalid)?;
        let consumer = function(consumer)?;
        let cancelled = function(cancelled)?;
        let hooks = match rest {
            [] | [Value::Undefined] => None,
            [Value::Data(v)] if v.is_null() || v.as_object().is_some_and(|o| o.is_empty()) => None,
            [Value::Record(hooks)] => Some(hooks),
            _ => return Err(invalid("invalid invocation hooks")),
        };
        let hook = |name| hooks.and_then(|h| h.get(name)).map(function).transpose();
        let caller = rutis_bridge::session::caller_row()
            .map(|r| r.ctx().cancellation_token())
            .unwrap_or_default();
        let cancel = caller.child_token();
        let mut state = Consumer {
            consume: consumer,
            prepare: hook("prepare")?,
            before_attempt: hook("beforeAttempt")?,
            waiting: hook("waiting")?,
            exception: None,
            cancel: cancel.clone(),
        };
        let owner = self.clone();
        Ok(Value::future(async move {
            let _end = cancel.clone().drop_guard();
            let closing = owner.driver.0.closing().unwrap_or_default();
            let invocation = owner.invoke(request, mode, &cancel, &mut state, consume);
            let result = {
                tokio::pin!(invocation);
                tokio::select! {
                    result = &mut invocation => result,
                    _ = async { tokio::select! {
                        _ = closing.cancelled() => {},
                        _ = call(&cancelled, vec![]) => {},
                    }} => { cancel.cancel(); invocation.await }
                }
            };
            if let Some(error) = state.exception {
                return Err(error);
            }
            match result {
                Ok(reply) => arg(&reply),
                Err(error) => {
                    let code = match error {
                        HostError::Cancelled(_) => "cancelled",
                        HostError::Failed { .. } => "failed",
                    };
                    Err(rutis_bridge::exports::failure(
                        "ModelHostError",
                        code,
                        error.to_string(),
                        Some(json!(error)),
                    ))
                }
            }
        }))
    }
}
