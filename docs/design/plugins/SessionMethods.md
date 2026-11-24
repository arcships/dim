# SessionMethods

Scope: session. Issue: #2026.

SessionMethods owns a Session's method table: the namespaces its parts register, who answers a name now, and the admission of one call. The table's call data — `MethodCall`, `MethodReply`, the handler's own error and the table's error kinds — is defined here once for both method tables. ManagementMethods (#2021) is the process table; Rpc, the registering parts and `session.methods` are callers. The current implementation is in `crates/dim-agent-capabilities/src/methods.rs` and is described in the [SessionMethods plugin page](../../architecture/rust-agent/plugins/SessionMethods.md).

## Use and scope: responsibility split

SessionMethods owns:

- **Registrations.** One namespace per registering row, its declarations and its call callback; a registration ends when its row unloads.
- **Admission.** One call: the name's owner, the interruption kind its declaration names, the caller identity, and the settlement of the calls a registration accepted.
- **Withdrawn namespaces.** The last row that held each namespace, so a call after a withdrawal answers `reloading` or `unavailable` instead of falling through to a parent namespace.
- **Reserved names.** The host's own roots, built-ins and management namespaces are refused at registration; the host supplies the check as a process face.
- **The method list.** The declarations of the namespaces registered now, each under its owning namespace.

Callers keep:

- What a method means: the registering part declares and implements its namespace.
- Routing: Rpc decides which table a wire name reaches and reads `sessionId` from the params.
- Client methods: Rpc serves `session.methods` and the built-ins; this table answers its own registrations.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part publishes a namespace for its row's lifetime | No: the lifetime, the duplicate check and the reserved-name check are the owner's | `register` |
| Rpc dispatches one call to the namespace that holds it | No: the lookup, the caller identity, the interruption and the withdrawn-row answer are the owner's | `call` |
| `session.methods` lists what a Session serves | No: the declarations and the withdrawn-row rule are the owner's | `methods` |
| The caller checks that a namespace is free before registering | Yes: `register` answers `duplicate` or `reserved`; a pre-check races the next registration | Business |
| Rpc routes a name between management and Session | Yes: management answers its own `call` and whether it holds a name; Rpc reads `sessionId` | Business |
| A provider notifies its caller | Yes: the handler has the caller identity and uses `callChannel` (#2021) | Business |

An operation or parameter exists only where the row says No.

## Caller example

A part registers from its `apply`; the registration belongs to that part's row:

```rust
methods.register(
    ctx,
    "session.input".into(),
    declarations,
    |call, cancelled, _progress| Box::pin(withdraw(call, cancelled)),
).await?;
```

Rpc dispatches one wire call; the caller identity is data, and the reply is `MethodReply` data:

```rust
let reply = match methods.call(
    method.into(),
    params,
    Some(CallerRef { connection_id, call_id }),
    cancelled.clone(),
).await {
    Ok(reply) => reply,
    Err(MethodError::UnknownMethod { .. }) => return Err(RpcError::unknown_method()),
    Err(MethodError::Reloading) => return Err(RpcError::reloading()),
    Err(MethodError::Unavailable) => return Err(RpcError::app(-32000, "service_unavailable", "The service is unavailable")),
    Err(MethodError::Interrupted) => return Err(RpcError::interrupted()),
    Err(MethodError::Invalid { .. }) => return Err(RpcError::invalid("Invalid request parameters")),
    Err(MethodError::Closed) => return Err(RpcError::closed()),
    Err(MethodError::Reserved { .. }) | Err(MethodError::Duplicate { .. }) => unreachable!("dispatch cannot register"),
};
```

`session.methods` composes the value with the management table:

```rust
let listed = methods.methods()?.into_iter().filter(|decl| !management.holds(&decl.name)).collect::<Vec<_>>();
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:reservedMethods` | The host's reserved names, consulted at registration; a host that names none reserves only the name grammar. |
| `P:callChannel` | The calling connection's per-call progress, addressed by the caller's `{connectionId, callId}`; the table builds the handler's `Progress` from it, and an in-process call has none. |
| Host port: the registering row's status | The row that registered a withdrawn namespace and its copy in this Session, read when a call answers `reloading` or `unavailable`. The host supplies the port, so the table names no public service for the read. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a namespace for the calling row's lifetime | `{namespace, declarations}` and a call callback | `{}` | None; the owner completes it | `reserved`, `duplicate`, `invalid`, `closed` |
| `call` | Dispatch one call to the namespace that holds it | `{method, params, caller?}` | `{reply}` | The calling connection closing ends the call as interrupted; the provider's unload ends it by its declaration (`report` or `drain`) | `unknown_method`, `reloading`, `unavailable`, `interrupted`, `invalid`, `closed` |
| `methods` | The declarations registered now | `{}` | `{methods}` | None | `closed` |

`caller` is `{connectionId, callId}`: the connection the call arrived on and the call id the reply ordering uses; absent for an in-process call. The table passes the caller identity to the handler unchanged and writes no socket. The handler receives a `Progress` for its call through `callChannel`, addressed by the same ids; an in-process call has none. The call callback is a direct parameter; it never runs inside the Timeline's commit lock; its future is dropped when the call is interrupted.

A handler's reply is `MethodReply` data: `{kind: "value", value}` or its own error as `{code, message}`. The table adds no meaning. An error reply reaches the client as an error whose `appError` is the handler's `code`, with the JSON-RPC code Rpc chooses for a service error (`invalid_params` → -32602, `unknown_method` → -32601, otherwise -32000).

### Errors

| Kind | Client `appError` | What the caller does next |
| --- | --- | --- |
| `reserved` | — | The namespace or a name is the host's; pick another name |
| `duplicate` | — | The namespace is registered already; the row must not register twice |
| `invalid` | `invalid_params` | The namespace or a declaration is outside the grammar, or the request is malformed |
| `unknown_method` | `unknown_method` | No registered or withdrawn namespace holds the name; the client sees an unknown method |
| `reloading` | `service_reloading` | The registering row is starting or reloading; retry after it is Active |
| `unavailable` | `service_unavailable` | The registering row failed or cannot run; the client sees the service as unavailable |
| `interrupted` | `operation_interrupted` | The provider unloaded while the call ran; the call may have committed, so read the current state before retrying |
| `closed` | `session_closed` | This Session is closing; a later generation serves the next Session |

`reserved` and `duplicate` answer a registration only, so no client sees them.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the grammar, the reserved names and the duplicate, record the declarations and the callback under the row | — | The registration is withdrawn and its row recorded | New registrations are refused; the table reloads with the Session | The registering row |
| `call` | Find the owner, admit the call, run the callback, return the reply; on unload interrupt `report` and wait for `drain` | The connection's close ends the call as interrupted; the handler's own work settles by its own rule | The call ends; the registration's accepted calls settle | A withdrawn namespace answers from its row's status; a table reload ends in-flight calls | The table for admission and settlement; the handler for its effects |
| `methods` | Read the registered namespaces' declarations | — | Nothing to release | Pending reads answer from the state they observed | SessionMethods |

## Exceptions with reasons

- The table reads the registering row's status through a host port: the status is the mechanism's record of the row, not another business owner's state, and the table cannot answer `reloading` or `unavailable` without it.
- A handler's progress and notifications are not written here: the caller identity is data and `callChannel` (#2021) addresses the connection by call id, so no connection, `Caller` or caller index crosses this owner.
- The table remembers the registering row per namespace: a withdrawn namespace must answer from that row rather than fall through to a parent namespace.

## Verification

Flow tests on the owner:

- A registration lives until its row unloads; a duplicate namespace, a reserved name and a malformed declaration are refused; a refused registration fails the registering part.
- `call` reaches the longest namespace holding the name; a withdrawn namespace answers `reloading` while the row starts and `unavailable` once it failed; a name no namespace ever held is `unknown_method`.
- A `report` call interrupted by the provider's unload replies `interrupted` and the provider's unload waits for it; a `drain` call settles with its own result.
- The caller identity reaches the handler unchanged for a wire call and is absent for an in-process call; the handler's notification with the same call id is written by `callChannel`.
- A value reply and an error reply reach the client unchanged: the error's `appError` is the handler's `code` and its JSON-RPC code follows the service-error rule.
- `methods` lists the registered declarations; Rpc's filter removes management-held names.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline fact and folds none, so it is not a Capability. It injects the host's reserved names and the call channel, and reads the registering row's status through a host port. |
| §2 scopes | Session: the table serves one Session; a branch part registers a Session namespace through this table only when its feature places the part in the Session. |
| §3 orthogonality | No other business owner's state is read or changed; the reserved names, the call channel and the row-status port are the mechanism's faces. |
| §4 rule 1 | No command event: a registration is the calling row's effect. |
| §4 rule 2 | No handoff: the reply is data; the handler's work settles with its registration. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | Core and assembly carry no method-specific meaning; a namespace's contents are its part's. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: a method's reply reaches the client through Rpc. |
| §4 rule 7 | Callers use `register`, `call` and `methods`; no caller sees the table, a provider or a channel. |
| §4 rule 8 | No infrastructure change; the withdrawn-row answer reads the mechanism's existing record through the port. |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and callbacks | The caller identity is data; the call callback crosses by reference; a registration lives with its row; no object, channel or provider crosses. |
| §7 no proxy | The table answers its own registrations; the host port answers the row status. |
| §7 one interface | One name, `sessionMethods`, on the owner type; the Rust binding and the wire call the same methods; the call data is defined here once; `SessionMethods` no longer derefs to a shared table type. |
| §7 cross-language | `register` crosses as data plus the caller's callback, `methods` as data, and `call` is Rpc's in-process call with the protocol's cancellation; no operation is a Rust-only convenience. |
| §7 one change | The implementation task deletes the old dispatch surface, adds these operations and migrates Rpc and every registering part in one change. |
| ADR 002 D1 | Session scope: namespaces belong to one Session's parts. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; `ForeignMethods` leaves. |
| ADR 002 D3 | No dependency on providers; a failed row affects only its namespaces. |
| ADR 002 D4 | The namespace set is structural: parts bring their namespaces. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. [#2021](https://github.com/arcships/dim/issues/2021): the process table serves the same `call {method, params, caller?}` request and `{reply}` shape, so Rpc has one call path for both tables.
2. [#2021](https://github.com/arcships/dim/issues/2021): the process table answers whether a registered or withdrawn namespace holds a name (today `MethodTable::holds`); Rpc routes a name by it and `session.methods` removes the names it holds.
3. [#2021](https://github.com/arcships/dim/issues/2021): `reservedMethods` stays a process face; the Session table consults `check {namespace, declarations}` at registration.
4. [#2021](https://github.com/arcships/dim/issues/2021): `callChannel` answers a progress write addressed by `{connectionId, callId}`; the table builds each call's `Progress` from it, because `call` takes the caller rather than a sink.
5. [#2021](https://github.com/arcships/dim/issues/2021): reply handoff (`handedOff`, `daemon.shutdown`) is not implemented; when it lands it is a variant of this shared `MethodReply`, and the process table's contract states it.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md#a1-method-contributions): the `MethodTable`/`MethodProvider`/`Accepted`/`Caller`/`CallSite`/`Callers` shapes and the Session table's `owner`/`provider`/`holds`/`listed` readers are replaced by `register`, `call` and `methods`; the `MethodCall`, `MethodReply` and `MethodError` shapes of A.1's interface block are defined here and A.1 links to them.

No client wire protocol or user-visible behavior changes: `register` keeps its wire form `register(namespace, declarations, call)` for a part of another language, and every client error keeps its application code.

## Binding appendix

The owner type `SessionMethods` carries the catalog name `sessionMethods` and serves other languages at its Session dispatcher key. `#[rutis::service]` cannot bind the face: `register` takes a callback and `call` a cancellation, so the dispatch is written by hand and calls the same operations. `MethodTable`, `ForeignMethods` and the `SessionMethods` deref leave; the table's storage is private. `MethodDecl`, `Interruption` and `Progress` are the capabilities crate's types; `RpcError` is its wire error, and the caller example above uses it. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is the binding's cancellation argument and is not a request field. The process table ([ManagementMethods](https://github.com/arcships/dim/issues/2021)) uses the same `MethodDecl`, `MethodCall`, `MethodReply`, `MethodCallbackError`, `MethodError`, `CallerRef` and `Interruption`.

```rust
use crate::host::CancelledFn;
use crate::methods::{MethodDecl, Progress};
use rutis::host::{BoxFuture, CallError};
use rutis::Ctx;
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue};
use tokio_util::sync::CancellationToken;

/// Who called: the connection the call arrived on and the call id the reply
/// ordering uses; absent for an in-process call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerRef {
    pub connection_id: String,
    pub call_id: String,
}

/// One admitted call as its handler receives it: the declared name, the raw
/// params and who called.
#[derive(Debug, Clone)]
pub struct MethodCall {
    pub method: String,
    pub params: serde_json::Value,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct Origin {
    pub caller: Option<CallerRef>,
}

/// A handler's own failure, carried to the client as its `code` and `message`.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct MethodCallbackError {
    pub code: String,
    pub message: String,
}

impl From<CallError> for MethodCallbackError {
    fn from(error: CallError) -> Self {
        match error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>()) {
            Some(rutis_bridge::session::Error::Remote { name, message, .. }) => Self { code: name.clone(), message: message.clone() },
            Some(other) => Self { code: other.code().unwrap_or("failed").into(), message: other.to_string() },
            None => Self { code: "failed".into(), message: error.message().into() },
        }
    }
}

/// The dispatch reply: the handler's value, or its own error as data. The
/// bridge writes a value as `{kind: "value", value}` and an error as
/// `{code, message}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum MethodReply {
    Value(ValueReply),
    Error(MethodCallbackError),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ValueReply {
    Value {
        #[serde(default)]
        value: serde_json::Value,
    },
}

impl From<MethodCallbackError> for MethodReply {
    fn from(error: MethodCallbackError) -> Self {
        Self::Error(error)
    }
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum MethodError {
    #[error("{message}")]
    Reserved { message: String },
    #[error("{message}")]
    Duplicate { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("no namespace holds {method}")]
    UnknownMethod { method: String },
    #[error("the registering row is reloading")]
    Reloading,
    #[error("the registering row is unavailable")]
    Unavailable,
    #[error("the provider unloaded during the call")]
    Interrupted,
    #[error("the Session is closing")]
    Closed,
}

/// The owner type; the namespaces, their declarations and their last rows
/// stay private.
pub struct SessionMethods { /* ... */ }

impl SessionMethods {
    /// Publishes `namespace` for the lifetime of the row that owns `caller`.
    pub async fn register(
        &self,
        caller: &Ctx,
        namespace: String,
        declarations: Vec<MethodDecl>,
        call: impl Fn(MethodCall, CancellationToken, Progress) -> BoxFuture<'static, Result<MethodReply, MethodCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), MethodError> { /* ... */ }

    pub async fn call(
        &self,
        method: String,
        params: serde_json::Value,
        caller: Option<CallerRef>,
        cancelled: CancelledFn,
    ) -> Result<MethodReply, MethodError> { /* ... */ } // builds MethodCall { method, params, origin }

    pub fn methods(&self) -> Result<Vec<MethodDecl>, MethodError> { /* ... */ }
}

/// The catalog name `sessionMethods` in the Session group, served to other
/// languages at its dispatcher key. `#[rutis::service]` cannot bind this face:
/// `register` carries a callback and `call` a cancellation, and neither is
/// data. The dispatch decodes the wire form and calls the same operations.
impl HostDispatch for SessionMethods {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "register": "async" }))
    }

    /// `register {namespace, declarations, call}`, registered for the bridge's
    /// calling row.
    fn invoke(&self, method: &str, args: BridgeValue) -> Reply { /* ... */ }
}
```

`MethodReply::Value` wraps `ValueReply::Value` so the wire keeps its `kind` tag; `MethodReply::Error` is the handler's `{code, message}` unchanged, and `From<MethodCallbackError>` produces it. `MethodError` serializes with `#[serde(tag = "code", rename_all = "snake_case")]`, so each variant's wire code is its snake-case name. `SessionMethods` derives the catalog name `sessionMethods` from the owner type, as it does today; `ForeignMethods` no longer exists and the dispatch above is the owner's own entry.
