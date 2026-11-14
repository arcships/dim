//! The plumbing of the process host-port faces: the one request object a call
//! carries, the wait a caller abandons, and the errors a face answers with.
//!
//! Each port writes its dispatcher over these helpers, so the wire shape and
//! its Rust binding are one module: the dispatcher accepts exactly the
//! documented request object, and its reply is a cancellable future — a caller
//! that drops its wait cancels the call, and the owner sees the token it was
//! given cancelled.
use rutis::host::{BoxFuture, Key};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reply, Value as BridgeValue, host_key,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The request of an operation that takes none: the documented `{}`.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// The one request object `args` carries, decoded as `T`.
pub(crate) fn request<T: DeserializeOwned>(
    port: &str,
    args: BridgeValue,
) -> Result<T, BridgeError> {
    let args = match args {
        BridgeValue::Undefined => Vec::new(),
        args => args
            .list()
            .map_err(|_| invalid(port, "the arguments are not a list"))?,
    };
    let [request] = args.as_slice() else {
        return Err(invalid(
            port,
            format!(
                "expected the request object, received {} arguments",
                args.len()
            ),
        ));
    };
    let request = request
        .clone()
        .json()
        .map_err(|_| invalid(port, "the request is not data"))?;
    serde_json::from_value(request).map_err(|error| invalid(port, error))
}

/// One operation's answer as the face serves it: a cancellable future whose
/// token is cancelled when the last caller drops its wait, so an abandoned
/// call ends the owner's read instead of letting it run on.
pub(crate) fn serving<T, E>(
    port: &'static str,
    cancel: CancellationToken,
    call: BoxFuture<'static, Result<T, E>>,
) -> Reply
where
    T: Serialize + Send + 'static,
    E: Serialize + std::fmt::Display + Send + 'static,
{
    let guard = cancel.clone().drop_guard();
    Ok(BridgeValue::cancellable_future(async move {
        let _abandoned = guard;
        match call.await {
            Ok(reply) => rutis_bridge::session::arg(&reply),
            Err(error) => Err(failure(port, error)),
        }
    }))
}

/// [`serving`] for an answer already encoded: a face whose operations answer
/// with different reply types encodes each one itself.
pub(crate) fn serving_encoded<E>(
    port: &'static str,
    cancel: CancellationToken,
    call: BoxFuture<'static, Result<BridgeValue, E>>,
) -> Reply
where
    E: Serialize + std::fmt::Display + Send + 'static,
{
    let guard = cancel.clone().drop_guard();
    Ok(BridgeValue::cancellable_future(async move {
        let _abandoned = guard;
        call.await.map_err(|error| failure(port, error))
    }))
}

/// One owner's error as its caller reads it: its serde `code`, and its text.
pub(crate) fn failure<E: Serialize + std::fmt::Display>(
    port: &'static str,
    error: E,
) -> BridgeError {
    let code = serde_json::to_value(&error)
        .ok()
        .and_then(|value| value.get("code")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "failed".into());
    rutis_bridge::exports::failure(port, &code, error.to_string(), None)
}

/// A caller's argument the port refuses: `invalid_request`.
pub(crate) fn invalid(port: &str, message: impl std::fmt::Display) -> BridgeError {
    rutis_bridge::exports::failure(port, "invalid_request", format!("{port}: {message}"), None)
}

/// A method the port does not serve.
pub(crate) fn unknown_method(port: &str, method: &str) -> BridgeError {
    BridgeError::Value(format!("{port} has no method {method}"))
}

/// The caller's side of one operation: the request it sends and the wait it
/// abandons. A cancelled `cancel` ends the call before it starts, and a wait
/// dropped mid-flight cancels the owner's read.
pub(crate) async fn call<T: DeserializeOwned>(
    port: &'static str,
    face: &Arc<dyn HostDispatch>,
    method: &str,
    request: &impl Serialize,
    cancel: &CancellationToken,
) -> Result<T, BridgeError> {
    if cancel.is_cancelled() {
        return Err(cancelled(port));
    }
    let args = BridgeValue::List(vec![rutis_bridge::session::arg(request)?]);
    let reply = face.invoke(method, args)?;
    let BridgeValue::Reference(wait) = reply else {
        return reply
            .json()
            .map_err(|_| invalid(port, "the reply is not data"))
            .and_then(|value| serde_json::from_value(value).map_err(|error| invalid(port, error)));
    };
    let reply = tokio::select! {
        reply = wait.wait_async() => reply?,
        _ = cancel.cancelled() => return Err(cancelled(port)),
    };
    let value = reply
        .json()
        .map_err(|_| invalid(port, "the reply is not data"))?;
    serde_json::from_value(value).map_err(|error| invalid(port, error))
}

fn cancelled(port: &str) -> BridgeError {
    rutis_bridge::exports::failure(port, "cancelled", format!("{port}: cancelled"), None)
}

/// The key a port type's face is served at: its name in camelCase, as a
/// marked service is named.
pub(crate) fn face_key<T: ?Sized>() -> Key<dyn HostDispatch> {
    host_key(&dim_agent_runtime::names::name_of::<T>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_carries_the_request_object_and_nothing_else() {
        let empty = rutis_bridge::session::arg(&Empty {}).unwrap();
        assert!(request::<Empty>("port", BridgeValue::List(vec![empty])).is_ok());
        assert!(request::<Empty>("port", BridgeValue::Undefined).is_err());
        assert!(
            request::<Empty>(
                "port",
                BridgeValue::List(vec![serde_json::json!({"unexpected": true}).into()])
            )
            .is_err()
        );
    }
}
