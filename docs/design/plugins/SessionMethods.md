# SessionMethods

Scope: session. Issue: #2026.

SessionMethods owns a Session's method table: the namespaces its parts register, who answers a name now, and the admission of one call. The call data both method tables serve — `MethodCall`, `MethodReply`, the handler's own error and the table's error kinds — has one definition, in the `methods` module of `dim-agent-capabilities`; this contract states it. ManagementMethods (#2021) is the process table; Rpc, the registering parts and `session.methods` are callers. The current implementation is in `crates/dim-agent-capabilities/src/methods.rs` and is described in the [SessionMethods plugin page](../../architecture/rust-agent/plugins/SessionMethods.md).

## Use and scope: responsibility split

SessionMethods owns:

- **Registrations.** One namespace per registration, its declarations and its call callback; a row may register several namespaces, and a registration ends when its row unloads.
- **Admission.** One call: the name's owner, the interruption kind its declaration names, the caller identity, and the settlement of the calls a registration accepted.
- **Withdrawn namespaces.** The last row that held each namespace, so a call after a withdrawal answers `reloading` or `unavailable` instead of falling through to a parent namespace.
- **Reserved names.** The host's own roots, built-ins and their notification names are refused at registration; a management namespace does not stop a Session namespace from registering (A.1 namespace collisions). The host supplies the check as a process face.
- **The method list.** The declarations of the namespaces registered now, each under its owning namespace.

Callers keep:

- What a method means: the registering part declares and implements its namespace.
- Routing: Rpc decides which table a wire name reaches and reads `sessionId` from the params.
- Client methods: Rpc serves `session.methods` and the built-ins; this table answers its own registrations.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part publishes a namespace for its row's lifetime | No: the lifetime, the duplicate check and the host's refusal are the owner's | `register` |
| Rpc dispatches one call to the namespace that holds it | No: the lookup, the caller identity, the interruption and the withdrawn-row answer are the owner's | `call` |
| `session.methods` lists what a Session serves | No: the declarations and the withdrawn-row rule are the owner's | `methods` |
| The caller learns why a registration was refused | Yes: `register` answers `duplicate` or `invalid`; a separate check would race the next registration | Business |
| Rpc routes a name between management and Session | Yes: the process table answers its own `call` and `held_namespaces`; Rpc reads `sessionId` | Business |
| A provider notifies its caller | Yes: a native handler has the call's `Caller`, and a handler of another language has `{connectionId, callId}` for `callChannel` (#2021) | Business |

An operation or parameter exists only where the row says No.

## Caller example

A part registers from its `apply`; the registration belongs to that part's row. A refused registration fails the part, so the part turns the `MethodError` into its own `rutis::Error`:

```rust
methods.register(
    ctx,
    "session.input".into(),
    declarations,
    |call, cancelled, _progress| Box::pin(withdraw(call, cancelled)),
)
.await
.map_err(|error| rutis::Error::from(rutis::host::ErrorKind::PluginFailed(error.to_string().into())))?;
```

Rpc dispatches one wire call; the caller is the admission's site for this call, `progress` is Rpc's sink for it, and the reply is `MethodReply` data:

```rust
let reply = match methods.call(
    method.into(),
    params,
    Some(site),
    cancelled.clone(),
    progress,
).await {
    Ok(reply) => reply,
    Err(MethodError::UnknownMethod { .. }) => return Err(RpcError::unknown_method()),
    Err(MethodError::Reloading) => return Err(RpcError::reloading()),
    Err(MethodError::Unavailable) => return Err(RpcError::app(-32000, "service_unavailable", "The service is unavailable")),
    Err(MethodError::Interrupted) => return Err(RpcError::interrupted()),
    Err(MethodError::Closed) => return Err(RpcError::closed()),
    Err(MethodError::Duplicate { .. }) | Err(MethodError::Invalid { .. }) => unreachable!("dispatch cannot register"),
};
```

`session.methods` composes the value with the process table's held namespaces:

```rust
// A.1's longest-namespace rule: a declaration whose name equals or lies inside
// a held management namespace is routed to management, never listed for the Session.
let Ok(held) = management.held_namespaces() else {
    return Err(RpcError::service("service_unavailable", "The process is closing"));
};
// `methods` fails only `closed`, which the client sees as `session_closed`.
let Ok(declarations) = methods.methods() else {
    return Err(RpcError::closed());
};
let listed = declarations.into_iter().filter(|decl| {
    let covered = |ns: &str| decl.name == ns || decl.name.strip_prefix(ns).is_some_and(|rest| rest.starts_with('.'));
    !held.iter().any(|namespace| covered(&namespace.namespace))
}).collect::<Vec<_>>();
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:reservedMethods` (optional) | The host's reserved names, consulted at registration. It is an `Option<Key<ReservedMethods>>` the factory fills only when the assembly names it; a host that names none reserves only the name grammar. |
| `rutis/loader` (rutis-loader's process face; App Server's loader row) | The row that registered a withdrawn namespace and its copies in this Session. The per-row read is `get(row)`, whose `EntryInfo.status` is the row's `EntryStatus`; `entries()` lists the row's copies with their instances. Read when a call answers `reloading` or `unavailable`. The face itself offers `status`, which builds the process's whole diagnostics snapshot for `rutis-host status`, so it answers neither one row's status nor its copies in one instance. The per-row read is therefore raised to the mechanism owner (§4 rule 8). |

The process table (#2021) serves the same `MethodCall`, `MethodReply` and `MethodError` data — its `call` takes the admission's `CallSite` and its `MethodCall.origin` carries the `Caller`, with this contract, recorded on #2021 — and answers `held_namespaces` so Rpc routes a name and filters `session.methods` by A.1's longest-namespace rule.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a namespace for the calling row's lifetime | `{namespace, declarations}` and a call callback | `{}` | None; the owner completes it | `invalid`, `duplicate`, `closed` |
| `call` | Dispatch one call to the namespace that holds it | `{method, params, caller?}` | `{reply}` | `cancelled` settles when the admitting Rpc load unloads; the table then interrupts the call: `report` answers `interrupted` and settles, `drain` replies with its own result. The provider's generation unload interrupts the same way. A caller that stops waiting drops the future and the accepted call continues; a disconnect cancels nothing. The progress sink is a binding parameter too | `unknown_method`, `reloading`, `unavailable`, `interrupted`, `closed` |
| `methods` | The declarations registered now | `{}` | `{methods}` | None | `closed` |

`register` and `methods` are data on the wire; `call` is the owner's in-process operation. In Rust each reply is a `Result`: `register` is `Result<(), MethodError>`, `call` is `Result<MethodReply, MethodError>`, and `methods` is `Result<Vec<MethodDecl>, MethodError>`. `{}` is the empty reply, `{methods}` the declaration vector, and `{reply}` the `MethodReply`. Rpc wraps a `MethodError` in its envelope; `MethodReply`'s own error is the handler's data and is not a table failure.

`caller` is the calling connection and this call's id; absent for an in-process call. Another table's wire carries it as `{connectionId, callId}`; in Rust the caller passes the admission's site (`CallSite`, below), which holds the connection and the call's close signal, and the table binds it to this generation's declared notifications. The handler receives it as `MethodCall.origin.caller`, A.2's `Caller`; a handler of another language receives `{connectionId, callId}`. The table writes no socket. It hands the handler a `Progress` over the call's sink; Rpc passes its connection's sink, and an in-process call passes one whose sends are dropped. The call callback is a direct parameter; it never runs inside the Timeline's commit lock; the handler's token fires on the provider's generation unload and on the admitting Rpc load's unload, which `cancelled` reports.

A handler's reply is `MethodReply` data: `{kind: "value", value}` or its own error as `{code, message}`. The table adds no meaning. The client sees the handler's failure in the Rpc envelope's error: `appError` is the handler's `code`, the message is the handler's `message`, the handler's `data` fields sit beside `appError`, and the JSON-RPC code is the one Rpc chooses for a service error (`invalid_params` → -32602, `unknown_method` → -32601, otherwise -32000).

### Errors

| Kind | Operation | Client `appError` | What the caller does next |
| --- | --- | --- | --- |
| `invalid` | `register` | — | The namespace or a declaration is refused: outside the name grammar, in a reserved root, equal to, containing or inside a built-in, a declared notification equal to a built-in notification's name, or a declaration that is malformed or repeated — the table checks the declarations and the host's check answers its reserved names. Fix the name or the declaration and register again |
| `duplicate` | `register` | — | The namespace is registered already; the row may register another namespace instead |
| `unknown_method` | `call` | `unknown_method` | No registered namespace holds the name, the name is outside the grammar, the owning namespace's provider does not declare it, or the row that registered its withdrawn namespace is disabled or absent; the client sees an unknown method |
| `reloading` | `call` | `service_reloading` | The registering row is starting or reloading; retry after it is Active |
| `unavailable` | `call` | `service_unavailable` | The registering row failed or cannot run. Retrying fails the same way until that row is fixed or restarted |
| `interrupted` | `call` | `operation_interrupted` | The provider's generation or the admitting Rpc load unloaded while the call ran; the call may have committed, so read the current state before retrying |
| `closed` | all | `session_closed` (Session table) | The table is closing; a later generation serves the next Session or the next process |

`invalid` and `duplicate` answer a registration only, so no client sees them. `closed` is one kind for both tables: the Session table's `closed` reaches a client as `session_closed`, and the process table has no client code of its own.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the host's refusal and the duplicate, record the declarations and the callback under the row | — | The registration is withdrawn and its row recorded | New registrations are refused; the table reloads with the Session | The registering row |
| `call` | Find the owner, admit the call, run the handler, return its reply; on unload interrupt `report` calls and wait for every accepted call | A caller that stops waiting drops the future; the accepted call settles by its declaration, and a disconnect cancels nothing | The registration's accepted calls settle by their declarations | A withdrawn namespace answers from its row's status; a table reload interrupts its calls by their declarations and waits for them | The table for admission and settlement; the handler for its effects |
| `methods` | Read the registered namespaces' declarations; the read observes the state at the call | — | Nothing to release | A table that closes answers no further read; a read already answered is unaffected | SessionMethods |

## Exceptions with reasons

- A handler error's owner fields cross to the client untouched: `MethodCallbackError` keeps them in `data`, and the table and Rpc never interpret or drop them ([Appendix C](../contracts/session-boundary.md#appendix-c-errors-and-recovery)).
- The call's connection crosses this owner twice: Rpc passes the admission's `CallSite` to `call`, and the table hands the handler the `Caller` it builds from it. No id alone can address a native row's connection, and a handler needs it to notify and to observe its close. [A.2](../contracts/session-boundary.md#a2-call-channel) fixes the behaviour and [Appendix F](../contracts/session-boundary.md#appendix-f-rust-binding) the two types: Rpc builds the site for one call and drops it when the call settles; the table binds the site to the serving generation's declared notifications; the handler may keep the `Caller` after the reply, it answers `undeclared`, `gone` or `too_large` per A.2, its `closed` resolves when the connection closes or the admitting Rpc load unloads, and neither type keeps a strong reference to the connection. A handler of another language receives `{connectionId, callId}` instead, because `callChannel` addresses the call by those ids.

## Verification

Flow tests on the owner:

- A registration lives until its row unloads; a duplicate namespace is `duplicate`, while a reserved name or a malformed declaration is `invalid`; a refused registration fails the registering part.
- `call` reaches the longest namespace holding the name; a withdrawn namespace answers `reloading` while the row starts and `unavailable` once it failed; a name no namespace ever held, and a name whose registering row is disabled or absent, are `unknown_method`.
- A `report` call interrupted by the provider's unload replies `interrupted` and the provider's unload waits for it; a `drain` call settles with its own result; a caller that disconnects while the call runs receives nothing further and the call still settles.
- A native handler receives the call's `Caller`, and a handler of another language receives `{connectionId, callId}`; an in-process call has no caller. The handler's notification is written by `callChannel` with the same call id, and its `closed` resolves when the connection closes or the admitting Rpc load unloads.
- A value reply and an error reply reach the client unchanged: the error's `appError` is the handler's `code` and its JSON-RPC code follows the service-error rule.
- A handler of another language that returns `{code, message}` reaches the client with that `appError`. A handler that fails with an `AgentError` reaches it with the code Rpc maps for that error. A runtime error converted from a `CallError` keeps its `code()`, else a remote's variant name; an error with no code and a call that crossed nothing are both `failed`.
- `methods` lists the registered declarations; Rpc's filter removes the names a held management namespace owns.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline fact and folds none, so it is not a Capability. It injects the host's reserved names and reads `rutis/loader`. |
| §2 scopes | Session: the table serves one Session; a branch part registers a Session namespace through this table only when its feature places the part in the Session. |
| §3 orthogonality | No other business owner's state is read or changed; `reservedMethods` and `rutis/loader` are the mechanism's faces. |
| §4 rule 1 | No command event: a registration is the calling row's effect. |
| §4 rule 2 | No handoff: the reply is data; the handler's work settles with its registration. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | Core and assembly carry no method-specific meaning; a namespace's contents are its part's. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: a method's reply reaches the client through Rpc. |
| §4 rule 7 | Callers use `register`, `call` and `methods`; no caller sees the registered namespaces, a provider or a channel through the owner. Rpc calls the operations and passes the call's site; a handler receives the `Caller`, the stated exception. |
| §4 rule 8 | The only infrastructure request is the loader's per-row read, raised below with its five fields; nothing else changes. |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and callbacks | The caller is data on the wire and A.2's `Caller` for a native handler — the one exception, stated with its shared design; the call callback, `cancelled` and the progress sink cross by reference; a registration lives with its row. |
| §7 no proxy | The table answers its own registrations; `rutis/loader`'s row status is the mechanism's read, raised below. |
| §7 one interface | One name, `sessionMethods`, on the owner type; `register` and `methods` are its wire operations, `call` is its in-process operation, and the Rust binding and the wire call the same functions. |
| §7 cross-language | `register` crosses as data plus the caller's callback through the table's dispatch, `methods` as data; `call` stays in process because it takes the admission's site, the cancellation and the progress sink, none of which is data; the site's type is below. |
| §7.1 resources and lifecycle | The table owns no I/O and no resource: the namespaces, their declarations and their last rows are memory. A registration lives until its row unloads; the table ends with its Session, and its admitted calls settle by their declarations first. |
| §7.1 operations | `register` is one state migration, `call` one call's admission and settlement, `methods` one read; each states its cancellation and its error kinds above, and no operation exposes a step another could be composed from. |
| §7.2 review checks | No cross-owner call is left unmapped; no object crosses except the stated site and `Caller`; the table does not answer for another owner; one interface; every operation names its errors; the delivery removes the old surface in one change. |
| §7 one change | The implementation task deletes the old dispatch surface, adds these operations and migrates Rpc and every registering part in one change. |
| ADR 002 D1 | Session scope: namespaces belong to one Session's parts. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; `ForeignMethods` leaves. |
| ADR 002 D3 | No dependency on providers; a failed row affects only its namespaces. |
| ADR 002 D4 | The namespace set is structural: parts bring their namespaces. |
| ADR 002 D5 | The wire operations and their data are language-neutral; `call` is the owner's in-process operation with the same reply data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

| Field | Value |
| --- | --- |
| Which feature is blocked, by which rule | The table answers `reloading` and `unavailable` for a withdrawn namespace, as [A.1](../contracts/session-boundary.md#a1-method-contributions) requires, from the status of the row that registered it and of that row's copy in this Session. §7 forbids reading another owner's state, and the face `rutis/loader` offers only `status` for the whole process, so the table has no per-row read. |
| Existing mechanisms considered | Rpc answering the two kinds instead of the table: rejected, the reviewed `call` answers them. Depending on `rutis/loader` as it is: blocked, its `status` builds the process's whole diagnostics snapshot (rows, services, runtimes, calls) and answers each row's own state, not a row's status and its copies in one instance; `dim-agent-capabilities` does not depend on `rutis-loader`. The table subscribing to loader changes and recording row status itself: a plugin-layer port, which §4 rule 8 forbids. |
| Generic capability needed | A data face over one loader row: `{row, instance}` → the row's `EntryStatus` and the status of its copy in that instance. No business meaning. |
| Effect on sequence, batches, interruption, reload | None on sequence or batches. A read, so it interrupts nothing. During a reload it answers the transitional status, which is its purpose. |
| Who implements it, which issue | The mechanism owner: rutis-loader's `rutis/loader` face and App Server's loader row (`crates/dim-app-server/src/lib.rs:1626`; the per-row read Rpc does today at `crates/dim-app-server/src/methods.rs:222-262`), tracked in [#2021](https://github.com/arcships/dim/issues/2021). |

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md#a1-method-contributions): the `MethodTable`/`MethodProvider`/`Accepted` shapes and the Session table's `owner`/`provider`/`holds`/`listed` readers are replaced by `register`, `call` and `methods`; `Caller`, `CallSite` and `Callers` stay the binding's, stated below and in A.2; the `MethodCall`, `MethodReply` and `MethodError` shapes of A.1's interface block are defined in `crate::methods` as stated here, and A.1 links to them.
- [session boundary Appendix F](../contracts/session-boundary.md#appendix-f-rust-binding): its method-table paragraphs — `Handler`, `MethodProvider`, `Check`, `Owner`, `MethodTable`, the `SessionMethods` deref, the `MethodCall`/`Origin` block and the foreign `register` decode — are the binding below; Appendix F links here and keeps only what CallChannel and ToolRegistry bind.

The client protocol and user-visible behavior stay as they are: `register` keeps its wire form `register(namespace, declarations, call)`, every client error keeps its application code, and a native handler already receives the `Caller`. Two changes touch a part of another language only: its `call` request nests the caller as `origin.caller` where today it sends a top-level `callId` and a flat `origin.connectionId`, and its handler's `{code, message, ...data}` becomes the client's `appError`, message and owner fields unchanged. An error a handler rethrows from a nested call keeps the code Rpc gives it, as today.

## Binding appendix

One definition, in the `methods` module of `dim-agent-capabilities`: the call data below — the declarations, the interruption, the reply, the errors, the progress sink and the admission's site. [A.2](../contracts/session-boundary.md#a2-call-channel) states the behaviour of `Caller`, `Progress` and the connection; this block states their shape and lifecycle, and `Caller` and `RpcError` are A.2's and [Appendix F](../contracts/session-boundary.md#appendix-f-rust-binding)'s own types, stated there. `MethodTable`, `ForeignMethods` and the `SessionMethods` deref leave; the table's storage is private. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is the binding's cancellation argument and is not a request field.

```rust
// crates/dim-agent-capabilities/src/methods.rs: the call data both tables serve.
use dim_agent_runtime::AgentError;
use rutis::host::{BoxFuture, CallError};
use tokio_util::sync::CancellationToken;

/// One declared method of a namespace; another language receives
/// `{name, mutating}`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodDecl {
    pub name: String,
    pub mutating: bool,
    #[serde(skip_serializing)]
    pub interruption: Interruption,
    #[serde(skip_serializing)]
    pub progress: bool,
    #[serde(default, skip_serializing)]
    pub notifications: Vec<String>,
    #[serde(default, skip_serializing)]
    pub reply: ReplyMode,
}

/// A method's reply mode (A.1). Reply handoff is not implemented, so nothing
/// reads it yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode {
    #[default]
    Direct,
    Handoff,
}

/// How a provider answers an admitted call that an unload interrupts: `report`
/// answers `interrupted` and settles, `drain` answers with its own result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interruption {
    Report,
    Drain,
}

/// The call's progress sink (A.2). The table hands it to a handler whose
/// declaration declares `progress` and closes it before the reply is written.
#[derive(Clone)]
pub struct Progress(/* the calling connection's sink */);

impl Progress {
    /// Sends one progress value; best effort, and dropped once the reply is
    /// handed over.
    pub fn send(&self, data: serde_json::Value) { /* ... */ }
}

/// One admitted call's connection, as Rpc passes it to `call`: the call's
/// channel, the connection's direct writer, the process's caller index (A.2)
/// and the connection's close signal. Rpc builds it for one call; the table
/// binds it to the serving generation's declarations.
#[derive(Clone)]
pub struct CallSite {
    pub connection: std::sync::Arc<dyn CallerConnection>,
    pub direct: std::sync::Arc<dyn CallerConnection>,
    pub callers: std::sync::Arc<Callers>,
    pub closed: CancellationToken,
}

/// One admitted call as its handler receives it: the declared name, the raw
/// params and who called.
#[derive(Clone)]
pub struct MethodCall {
    pub method: String,
    pub params: serde_json::Value,
    pub origin: Origin,
}

/// Who called: the call's `Caller` (A.2) for a native handler, built by the
/// admission from the calling connection and this generation's declared
/// notifications; Appendix F holds the type and its lifecycle.
#[derive(Clone)]
pub struct Origin {
    pub caller: Option<Caller>,
}

/// A handler's own failure: its application code, message and any owner-defined
/// fields the client receives beside `data.appError`.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct MethodCallbackError {
    pub code: String,
    pub message: String,
    /// Owner-defined wire fields, preserved by the table and Rpc; they must
    /// not shadow `appError` or a built-in field.
    #[serde(flatten, default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub data: std::collections::BTreeMap<String, serde_json::Value>,
}

impl MethodCallbackError {
    /// The client code, message and owner fields Rpc gives a runtime error.
    fn from_wire(wire: RpcError) -> Self {
        let data = wire.data.as_ref();
        Self {
            code: data.map(|data| data.app_error.clone()).unwrap_or_else(|| "failed".into()),
            message: wire.message,
            data: data.map(|data| data.details.clone()).unwrap_or_default(),
        }
    }
}

impl From<AgentError> for MethodCallbackError {
    fn from(error: AgentError) -> Self {
        Self::from_wire(RpcError::from(error))
    }
}

/// A handler's own `params(..)?` or `reply(..)?` failure keeps its client
/// code, message and owner fields.
impl From<RpcError> for MethodCallbackError {
    fn from(wire: RpcError) -> Self {
        Self::from_wire(wire)
    }
}

impl From<CallError> for MethodCallbackError {
    fn from(error: CallError) -> Self {
        let remote = error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>());
        // An error a nested call rethrew keeps the client code Rpc gives it,
        // `AgentError`'s mapping included. `rethrown` is this module's
        // existing conversion: `fn rethrown(&session::Error) -> Option<RpcError>`.
        if let Some(wire) = remote.and_then(rethrown) {
            return Self::from_wire(wire);
        }
        // Otherwise the application code the error carries; a remote's variant
        // name without one, and `failed` for a call that crossed nothing.
        let code = remote
            .and_then(|error| {
                error.code().map(str::to_owned).or_else(|| match error {
                    rutis_bridge::session::Error::Remote { name, .. } => Some(name.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "failed".into());
        Self { code, message: error.message().into(), data: Default::default() }
    }
}

/// The handler both tables store: the call, the serving generation's
/// cancellation token and the call's progress sink.
pub type Handler = std::sync::Arc<
    dyn Fn(MethodCall, CancellationToken, Progress)
            -> BoxFuture<'static, Result<MethodReply, MethodCallbackError>>
        + Send
        + Sync,
>;

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

impl From<serde_json::Value> for MethodReply {
    fn from(value: serde_json::Value) -> Self {
        Self::Value(ValueReply::Value { value })
    }
}

impl From<MethodCallbackError> for MethodReply {
    fn from(error: MethodCallbackError) -> Self {
        Self::Error(error)
    }
}

/// The table's own failures, by kind. One kind set serves both tables;
/// `closed` reaches a client as `session_closed` for the Session table, and
/// the process table has no client code of its own.
#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum MethodError {
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
    #[error("the provider's generation or the admitting Rpc load unloaded during the call")]
    Interrupted,
    #[error("the table is closing")]
    Closed,
}
```

```rust
// crates/dim-agent-capabilities/src/methods.rs: the owner, in the same module.
use crate::host::CancelledFn;
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

    /// One call, in process: the caller's site (`CallSite`, Appendix F), the
    /// cancellation and the progress sink are parameters, and the reply is
    /// data.
    #[service(skip)]
    pub async fn call(
        &self,
        method: String,
        params: serde_json::Value,
        caller: Option<CallSite>,
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

`register` and `methods` are the wire face: `register` through the dispatch above, which decodes the declarations and the caller's function for the bridge's calling row, and `methods` through the marked service and that dispatch. `call` is the owner's in-process operation: it takes the admission's site, the cancellation and the progress sink, so none of them crosses a wire. `MethodCall` carries A.2's `Caller` to a native handler and `{connectionId, callId}` to a handler of another language. `MethodReply::Value` wraps `ValueReply::Value` so the wire keeps its `kind` tag; `MethodReply::Error` is the handler's `{code, message}` unchanged, and `From<MethodCallbackError>` produces it. `MethodError` serializes with `#[serde(tag = "code", rename_all = "snake_case")]`, so each variant's wire code is its snake-case name; the client's `appError` is chosen by Rpc from the variant and never carried in the table's error data; the handler's `code` becomes it on the envelope.
