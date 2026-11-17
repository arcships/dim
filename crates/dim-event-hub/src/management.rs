//! The `eventhub` management namespace (design §6, A.3), registered the way
//! Scheduler registers `schedule`: servers of protected-local connections
//! only.
use crate::{
    hub::Shared,
    model::{ListRequest, Origin, SubscribeRequest, UnsubscribeRequest},
};
use dim_agent_capabilities::methods::{
    Handler, Interruption, MethodCall, MethodDecl, RpcError, params,
};
use serde_json::Value;
use std::sync::Arc;

pub(crate) const NAMESPACE: &str = "eventhub";

/// The namespace's methods, in the design's order (design A.3).
pub(crate) fn declarations() -> Vec<MethodDecl> {
    vec![
        MethodDecl::new("eventhub.sources", false, Interruption::Report),
        MethodDecl::new("eventhub.subscriptions", false, Interruption::Report),
        MethodDecl::new("eventhub.subscribe", true, Interruption::Drain),
        MethodDecl::new("eventhub.unsubscribe", true, Interruption::Drain),
    ]
}

pub(crate) fn handler(shared: Arc<Shared>) -> Handler {
    Arc::new(move |call, _cancel, _progress| {
        let MethodCall {
            method,
            params: value,
            origin: _,
        } = call;
        let shared = shared.clone();
        Box::pin(async move { dispatch(&shared, &method, value).await })
    })
}

async fn dispatch(shared: &Arc<Shared>, method: &str, value: Value) -> Result<Value, RpcError> {
    match method {
        "eventhub.sources" => {
            let _: EmptyParams = params(value)?;
            Ok(shared.sources())
        }
        "eventhub.subscriptions" => {
            let input: ListRequest = params(value)?;
            Ok(shared.subscriptions(input))
        }
        "eventhub.subscribe" => {
            let input: SubscribeRequest = params(value)?;
            Ok(shared.subscribe(Origin::Client, input).await?)
        }
        "eventhub.unsubscribe" => {
            let input: UnsubscribeRequest = params(value)?;
            Ok(shared.unsubscribe(input).await?)
        }
        _ => Err(RpcError::unknown_method()),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

impl From<crate::error::EventHubError> for RpcError {
    fn from(error: crate::error::EventHubError) -> Self {
        RpcError::service(error.code, error.message)
    }
}
