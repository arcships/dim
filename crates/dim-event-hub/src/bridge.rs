//! The `eventHub` and `eventSubscriptions` names for other languages (design
//! Appendix F): a `HostDispatch` at each `host_key`, whose operations are
//! `EventHub`'s and `EventSubscriptions`' own. `declare` and `publish` take
//! the calling row from `caller_row()`.
use crate::{error::EventHubError, hub::Shared, model::PublishRequest};
use rutis::Ctx;
use rutis_bridge::exports::failure;
use rutis_bridge::session::{Error as BridgeError, HostDispatch, Reply, Value};
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};
use std::sync::Arc;

pub(crate) struct ForeignEventHub {
    pub(crate) shared: Arc<Shared>,
}

impl HostDispatch for ForeignEventHub {
    fn methods(&self) -> Option<Json> {
        Some(json!({ "declare": "async", "publish": "async" }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        // Read the caller inside `invoke`: a future returned here runs later
        // and no longer sees it.
        let caller = rutis_bridge::session::caller_row()
            .map(|row| row.ctx().clone())
            .ok_or_else(|| {
                failure(
                    "EventHubError",
                    "no_caller_row",
                    "declare and publish need the calling row's context: only a row of a language runtime can call them"
                        .into(),
                    None,
                )
            })?;
        let shared = self.shared.clone();
        match method {
            "declare" => {
                let request = request(args)?;
                Ok(Value::future(async move {
                    shared
                        .declare(&caller, request)
                        .await
                        .map(|()| Value::Undefined)
                        .map_err(bridge_error)
                }))
            }
            "publish" => {
                let request: PublishRequest = request(args)?;
                Ok(Value::future(async move {
                    crate::publish::publish(shared, &caller, request)
                        .await
                        .map(Into::into)
                        .map_err(bridge_error)
                }))
            }
            _ => Err(BridgeError::Value(format!(
                "eventHub has no method {method}"
            ))),
        }
    }
}

/// The single request object a bridge call carries.
fn request<T: DeserializeOwned>(args: Value) -> Result<T, BridgeError> {
    let serde_json::Value::Array(args) = args.json()? else {
        return Err(BridgeError::Value("expected one request object".into()));
    };
    let Ok([request]) = <[serde_json::Value; 1]>::try_from(args) else {
        return Err(BridgeError::Value("expected one request object".into()));
    };
    serde_json::from_value(request).map_err(|error| BridgeError::Value(error.to_string()))
}

fn bridge_error(error: EventHubError) -> BridgeError {
    failure("EventHubError", error.code, error.message, None)
}

/// The `eventHub` service's Rust type: `names::name_of::<EventHub>()` is
/// `eventHub` (design Appendix F). Producers read it with
/// `ctx.get_at(&names.key::<EventHub>()?)`.
pub struct EventHub {
    pub(crate) shared: Arc<Shared>,
}

impl EventHub {
    /// Declares a source for the row that owns `ctx` (design A.1).
    pub async fn declare(
        &self,
        ctx: &Ctx,
        request: crate::model::DeclareRequest,
    ) -> Result<(), EventHubError> {
        self.shared.declare(ctx, request).await
    }

    /// Publishes one event for the row that owns `ctx` (design A.2).
    pub async fn publish(
        &self,
        ctx: &Ctx,
        request: crate::model::PublishRequest,
    ) -> Result<Json, EventHubError> {
        crate::publish::publish(self.shared.clone(), ctx, request).await
    }
}

/// The `eventSubscriptions` name for other languages (design Appendix F): the
/// operations are the typed service's own, and the Session each names is a
/// value, as every data face's ids are.
pub(crate) struct ForeignEventSubscriptions {
    pub(crate) service: Arc<crate::subscriptions::EventSubscriptions>,
}

impl HostDispatch for ForeignEventSubscriptions {
    fn methods(&self) -> Option<Json> {
        Some(json!({
            "sources": "sync",
            "subscribe": "async",
            "unsubscribe": "async",
            "list": "sync",
        }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let service = self.service.clone();
        match method {
            "sources" => Ok(service.sources().into()),
            "subscribe" => {
                let call: crate::SubscriptionCall = request(args)?;
                Ok(Value::future(async move {
                    service
                        .subscribe(call)
                        .await
                        .map(Into::into)
                        .map_err(bridge_error)
                }))
            }
            "unsubscribe" => {
                let call: crate::UnsubscribeCall = request(args)?;
                Ok(Value::future(async move {
                    service
                        .unsubscribe(call)
                        .await
                        .map(Into::into)
                        .map_err(bridge_error)
                }))
            }
            "list" => {
                let call: crate::ListCall = request(args)?;
                Ok(service.list(call).into())
            }
            _ => Err(BridgeError::Value(format!(
                "eventSubscriptions has no method {method}"
            ))),
        }
    }
}
