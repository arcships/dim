# SessionMethods

Scope: session. Issue: #2026.

SessionMethods owns a Session's method table: the namespaces its parts register, who answers a name now, and the admission of one call. The call data both method tables serve — `MethodCall`, `MethodReply`, the handler's own error and the table's error kinds — has one definition, in the `methods` module of `dim-agent-capabilities`; this contract states it. ManagementMethods (#2021) is the process table; Rpc, the registering parts and `session.methods` are callers. The current implementation is in `crates/dim-agent-capabilities/src/methods.rs` and is described in the [SessionMethods plugin page](../../architecture/rust-agent/plugins/SessionMethods.md).

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
| Rpc routes a name between management and Session | Yes: the process table answers its own `call` and `held_namespaces`; Rpc reads `sessionId` | Business |
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

Rpc dispatches one wire call; the caller identity is data, the call's sink is the connection's, and the reply is `MethodReply` data:

```rust
let reply = match methods.call(
    method.into(),
    params,
    Some(CallerRef { connection_id, call_id: call_id.clone() }),
    cancelled.clone(),
    move |data| connection.progress(call_id.clone(), data),
).await {
    Ok(reply) => reply,
    Err(MethodError::UnknownMethod { .. }) => return Err(RpcError::unknown_method()),
    Err(MethodError::Reloading) => return Err(RpcError::reloading()),
    Err(MethodError::Unavailable) => return Err(RpcError::app(-32000, "service_unavailable", "The service is unavailable")),
    Err(MethodError::Interrupted) => return Err(RpcError::interrupted()),
    Err(MethodError::Closed) => return Err(RpcError::closed()),
    Err(MethodError::Reserved { .. }) | Err(MethodError::Duplicate { .. }) | Err(MethodError::Invalid { .. }) => unreachable!("dispatch cannot register"),
};
```

`session.methods` composes the value with the process table's held namespaces:

```rust
// A.1's longest-namespace rule: a name a held management namespace equals or
// prefixes is routed to management and is never listed for the Session.
let held = management.held_namespaces();
let listed = methods.methods()?.into_iter().filter(|decl| {
    let covered = |ns: &str| decl.name == ns || decl.name.strip_prefix(ns).is_some_and(|rest| rest.starts_with('.'));
    !held.iter().any(|namespace| covered(&namespace.namespace))
}).collect::<Vec<_>>();
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:reservedMethods` | The host's reserved names, consulted at registration; a host that names none reserves only the name grammar. |
| `P:loader` (`rutis_loader::Loader`, App Server's loader row) | The row that registered a withdrawn namespace and its copy in this Session: `get(row)` answers the row's `EntryStatus`, and `entries()` lists its copies with their instances. Read when a call answers `reloading` or `unavailable`; the name is `internal(owner-api)` today, so the read is raised to the mechanism owner (§4 rule 8). |

The process table (#2021) serves the same `MethodCall`, `MethodReply` and `MethodError` data, and answers `held_namespaces` so Rpc routes a name and filters `session.methods` by A.1's longest-namespace rule; its contract states both.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a namespace for the calling row's lifetime | `{namespace, declarations}` and a call callback | `{}` | None; the owner completes it | `reserved`, `duplicate`, `invalid`, `closed` |
| `call` | Dispatch one call to the namespace that holds it | `{method, params, caller?}` | `{reply}` | `cancelled` and the call's progress sink are binding parameters, not request fields. The admitting Rpc load's unload and the provider's generation unload interrupt the call: `report` answers `interrupted` and settles, `drain` replies with its own result. A caller disconnect cancels nothing | `unknown_method`, `reloading`, `unavailable`, `interrupted`, `closed` |
| `methods` | The declarations registered now | `{}` | `{methods}` | None | `closed` |

`caller` is `{connectionId, callId}`: the connection the call arrived on and the call id the reply ordering uses; absent for an in-process call. The table passes the caller identity to the handler unchanged and writes no socket. The table hands the handler a `Progress` over the call's sink; Rpc passes its connection's sink, and an in-process call passes one whose sends are dropped. The call callback is a direct parameter; it never runs inside the Timeline's commit lock; the handler's token fires on the provider's unload and on the admitting Rpc load's.

A handler's reply is `MethodReply` data: `{kind: "value", value}` or its own error as `{code, message}`. The table adds no meaning. The client sees the handler's failure in the Rpc envelope's error: `appError` is the handler's `code`, its message the handler's `message`, and the JSON-RPC code is the one Rpc chooses for a service error (`invalid_params` → -32602, `unknown_method` → -32601, otherwise -32000).

### Errors

| Kind | Operation | Client `appError` | What the caller does next |
| --- | --- | --- | --- |
| `reserved` | `register` | — | The namespace or a name is the host's; pick another name |
| `duplicate` | `register` | — | The namespace is registered already; the row must not register twice |
| `invalid` | `register` | `invalid_params` | The namespace or a declaration is outside the grammar |
| `unknown_method` | `call` | `unknown_method` | No registered namespace holds the name, the name is outside the grammar, or the row that registered its withdrawn namespace is disabled or absent; the client sees an unknown method |
| `reloading` | `call` | `service_reloading` | The registering row is starting or reloading; retry after it is Active |
| `unavailable` | `call` | `service_unavailable` | The registering row failed or cannot run; the client sees the service as unavailable |
| `interrupted` | `call` | `operation_interrupted` | The provider unloaded while the call ran; the call may have committed, so read the current state before retrying |
| `closed` | all | `session_closed` | This Session is closing; a later generation serves the next Session |

`reserved`, `duplicate` and `invalid` answer a registration only, so no client sees them.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the grammar, the reserved names and the duplicate, record the declarations and the callback under the row | — | The registration is withdrawn and its row recorded | New registrations are refused; the table reloads with the Session | The registering row |
| `call` | Find the owner, admit the call, run the handler, return its reply; on unload interrupt `report` calls and wait for every accepted call | A disconnect cancels nothing; the accepted call settles by its declaration | The registration's accepted calls settle by their declarations | A withdrawn namespace answers from its row's status; a table reload interrupts its calls by their declarations and waits for them | The table for admission and settlement; the handler for its effects |
| `methods` | Read the registered namespaces' declarations | — | Nothing to release | Pending reads answer from the state they observed | SessionMethods |

## Exceptions with reasons

- The table reads the registering row's status through `rutis/loader`: the status is the mechanism's record of the row, not another business owner's state, and the table cannot answer `reloading` or `unavailable` without it. The name is `internal(owner-api)` today; §4 rule 8 records the read.
- A handler's notifications are not written here: the caller identity is data and `callChannel` (#2021) addresses the connection by call id, so no connection, `Caller` or caller index crosses this owner.
- The table remembers the registering row per namespace: a withdrawn namespace must answer from that row rather than fall through to a parent namespace.

## Verification

Flow tests on the owner:

- A registration lives until its row unloads; a duplicate namespace, a reserved name and a malformed declaration are refused; a refused registration fails the registering part.
- `call` reaches the longest namespace holding the name; a withdrawn namespace answers `reloading` while the row starts and `unavailable` once it failed; a name no namespace ever held, and a name whose registering row is disabled or absent, are `unknown_method`.
- A `report` call interrupted by the provider's unload replies `interrupted` and the provider's unload waits for it; a `drain` call settles with its own result; a caller that disconnects while the call runs receives nothing further and the call still settles.
- The caller identity reaches the handler unchanged for a wire call and is absent for an in-process call; the handler's notification with the same call id is written by `callChannel`.
- A value reply and an error reply reach the client unchanged: the error's `appError` is the handler's `code` and its JSON-RPC code follows the service-error rule.
- `methods` lists the registered declarations; Rpc's filter removes the names a held management namespace owns.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline fact and folds none, so it is not a Capability. It injects the host's reserved names and reads the loader's row status. |
| §2 scopes | Session: the table serves one Session; a branch part registers a Session namespace through this table only when its feature places the part in the Session. |
| §3 orthogonality | No other business owner's state is read or changed; the reserved names and the loader's row status are the mechanism's faces. |
| §4 rule 1 | No command event: a registration is the calling row's effect. |
| §4 rule 2 | No handoff: the reply is data; the handler's work settles with its registration. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | Core and assembly carry no method-specific meaning; a namespace's contents are its part's. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: a method's reply reaches the client through Rpc. |
| §4 rule 7 | Callers use `register`, `call` and `methods`; no caller sees the table, a provider or a channel. |
| §4 rule 8 | No infrastructure change in this contract: the withdrawn-row answer uses the loader's existing row status, whose read is raised to the mechanism owner (see below). |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and callbacks | The caller identity is data; the call callback, `cancelled` and the progress sink cross by reference; a registration lives with its row; no object, channel or provider crosses. |
| §7 no proxy | The table answers its own registrations; the loader answers the row status. |
| §7 one interface | One name, `sessionMethods`, on the owner type; `register` and `methods` are its wire operations, `call` is its in-process operation, and the Rust binding and the wire call the same functions. |
| §7 cross-language | `register` crosses as data plus the caller's callback through the table's dispatch, `methods` as data; `call` stays in process because it takes the progress sink and the cancellation, neither of which is data. |
| §7.1 resources and lifecycle | The table owns no I/O and no resource: the namespaces, their declarations and their last rows are memory. A registration lives until its row unloads; the table ends with its Session, and its admitted calls settle by their declarations first. |
| §7.1 operations | `register` is one state migration, `call` one call's admission and settlement, `methods` one read; each states its cancellation and its error kinds above, and no operation exposes a step another could be composed from. |
| §7.2 review checks | No cross-owner call is left unmapped; no object or handle crosses; the table does not answer for another owner; one interface; every operation names its errors; the delivery removes the old surface in one change. |
| §7 one change | The implementation task deletes the old dispatch surface, adds these operations and migrates Rpc and every registering part in one change. |
| ADR 002 D1 | Session scope: namespaces belong to one Session's parts. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; `ForeignMethods` leaves. |
| ADR 002 D3 | No dependency on providers; a failed row affects only its namespaces. |
| ADR 002 D4 | The namespace set is structural: parts bring their namespaces. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

| Field | Value |
| --- | --- |
| Which feature is blocked, by which rule | The table answers `reloading` and `unavailable` for a withdrawn namespace, as [A.1](../contracts/session-boundary.md#a1-method-contributions) requires, from the status of the row that registered it and of that row's copy in this Session. §7 forbids reading another owner's state, and `loader` is `internal(owner-api)` in the catalog, so the table has no readable face for it. |
| Existing mechanisms considered | Rpc answering the two kinds instead of the table: rejected, the reviewed `call` answers them. Depending on the public `loader` name: blocked, it is `internal(owner-api)` and `dim-agent-capabilities` does not depend on `rutis-loader`. The table subscribing to loader changes and recording row status itself: a plugin-layer port, which §4 rule 8 forbids. |
| Generic capability needed | A data face over one loader row: `{row, instance}` → the row's `EntryStatus` and the status of its copy in that instance. No business meaning. |
| Effect on sequence, batches, interruption, reload | None on sequence or batches. A read, so it interrupts nothing. During a reload it answers the transitional status, which is its purpose. |
| Who implements it, which issue | The mechanism owner: App Server's loader row and rutis-loader (`loader`, `crates/dim-app-server/src/lib.rs:1626`; the read Rpc does today at `crates/dim-app-server/src/methods.rs:222-262`), tracked in [#2021](https://github.com/arcships/dim/issues/2021). |

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md#a1-method-contributions): the `MethodTable`/`MethodProvider`/`Accepted`/`Caller`/`CallSite`/`Callers` shapes and the Session table's `owner`/`provider`/`holds`/`listed` readers are replaced by `register`, `call` and `methods`; the `MethodCall`, `MethodReply` and `MethodError` shapes of A.1's interface block are defined in `crate::methods` as stated here, and A.1 links to them.
- [session boundary Appendix F](../contracts/session-boundary.md#appendix-f-rust-binding): its method-table paragraphs — `Handler`, `MethodProvider`, `Check`, `Owner`, `MethodTable`, the `SessionMethods` deref, the `MethodCall`/`Origin` block and the foreign `register` decode — are the binding below; Appendix F links here and keeps only what CallChannel and ToolRegistry bind.

No client wire protocol or user-visible behavior changes: `register` keeps its wire form `register(namespace, declarations, call)` for a part of another language, and every client error keeps its application code.

## Binding appendix

One definition, in the `methods` module of `dim-agent-capabilities`: the call data below, beside `MethodDecl`, `Interruption` and `Progress`. `MethodTable`, `ForeignMethods` and the `SessionMethods` deref leave; the table's storage is private. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is the binding's cancellation argument and is not a request field.

```rust
// crates/dim-agent-capabilities/src/methods.rs: the call data both tables serve.
use rutis::host::CallError;

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

/// A handler's own failure: its application code and message, which the client
/// sees as the Rpc envelope's `appError` and message.
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

/// The handler's reply: its value, or its own failure as data. The bridge
/// writes a value as `{kind: "value", value}` and a failure as `{code, message}`.
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

/// The table's own failures, by kind.
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
```

```rust
// crates/dim-agent-capabilities/src/methods.rs: the owner, in the same module.
use crate::host::CancelledFn;
use crate::methods::{MethodDecl, Progress};
use rutis::host::BoxFuture;
use rutis::Ctx;
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue};
use tokio_util::sync::CancellationToken;

/// The owner type; the namespaces, their declarations and their last rows
/// stay private.
pub struct SessionMethods { /* ... */ }

#[rutis::service(name = "sessionMethods")]
impl SessionMethods {
    /// Publishes `namespace` for the lifetime of the row that owns `caller`.
    /// The macro leaves it to the dispatch below: the callback's arguments are
    /// not data.
    #[service(skip)]
    pub async fn register(
        &self,
        caller: &Ctx,
        namespace: String,
        declarations: Vec<MethodDecl>,
        call: impl Fn(MethodCall, CancellationToken, Progress) -> BoxFuture<'static, Result<MethodReply, MethodCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), MethodError> { /* ... */ }

    /// The declarations registered now.
    pub fn methods(&self) -> Result<Vec<MethodDecl>, MethodError> { /* ... */ }

    /// One call, in process: the cancellation and the progress sink are
    /// parameters, and the reply is data.
    #[service(skip)]
    pub async fn call(
        &self,
        method: String,
        params: serde_json::Value,
        caller: Option<CallerRef>,
        cancelled: CancelledFn,
        progress: impl Fn(serde_json::Value) + Send + Sync + 'static,
    ) -> Result<MethodReply, MethodError> { /* ... */ }
}

/// Another language reaches the table at its Session dispatcher key:
/// `register {namespace, declarations, call}` and `methods {}`.
impl HostDispatch for SessionMethods {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "register": "async", "methods": "sync" }))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply { /* ... */ }
}
```

`register` and `methods` are the wire face: `register` through the dispatch above, which decodes the declarations and the caller's function for the bridge's calling row, and `methods` through the marked service and that dispatch. `call` is the owner's in-process operation, so its cancellation and progress sink never cross a wire. `MethodReply::Value` wraps `ValueReply::Value` so the wire keeps its `kind` tag; `MethodReply::Error` is the handler's `{code, message}` unchanged, and `From<MethodCallbackError>` produces it. `MethodError` serializes with `#[serde(tag = "code", rename_all = "snake_case")]`, so each variant's wire code is its snake-case name; the client's `appError` is chosen by Rpc from the variant, never carried in the data.
