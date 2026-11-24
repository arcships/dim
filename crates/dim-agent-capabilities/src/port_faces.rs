//! Data encoding at the process host-port boundary and cancellation of
//! typed owner reads. Native calls do not pass through the dispatcher.
use rutis::host::Key;
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reply, Value as BridgeValue, host_key,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
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
    if !request.is_object() {
        return Err(invalid(port, "expected a request object"));
    }
    serde_json::from_value(request).map_err(|error| invalid(port, error))
}

/// Encodes a typed operation's reply for a caller across the bridge.
/// Abandoning the wait drops the operation and its cancellation guard.
pub(crate) fn serving<T, E>(
    port: &'static str,
    call: impl Future<Output = Result<T, E>> + Send + 'static,
) -> Reply
where
    T: Serialize + Send + 'static,
    E: Serialize + std::fmt::Display + Send + 'static,
{
    Ok(BridgeValue::cancellable_future(async move {
        match call.await {
            Ok(reply) => rutis_bridge::session::arg(&reply),
            Err(error) => Err(failure(port, error)),
        }
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
    BridgeError::value(format!("{port} has no method {method}"))
}

/// Runs an owner read in the caller's task. Cancelling or dropping the call
/// cancels only its child token; a pre-cancelled call never reaches the owner.
pub(crate) async fn read<T, E, F: Future<Output = Result<T, E>>>(
    cancel: CancellationToken,
    cancelled: impl FnOnce() -> E,
    read: impl FnOnce(CancellationToken) -> F,
) -> Result<T, E> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let child = cancel.child_token();
    let _abandoned = child.clone().drop_guard();
    tokio::select! {
        result = read(child) => result,
        _ = cancel.cancelled() => Err(cancelled()),
    }
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
