# CallChannel

Scope: process. Issue: #2021.

CallChannel owns the binding between an admitted table call and the connection it arrived on: which notifications the provider may send, whether a notification is declared, and when the connection is gone. This document is the owner's target contract; the current implementation is `CallChannelPlugin` in `crates/dim-app-server/src/connection.rs` and `Caller`/`Callers` in `crates/dim-agent-capabilities/src/methods.rs`. Connection, Rpc and the method tables are separate owners. The notification, progress and closed semantics — modes, refusal order and lifetime — are the [methods contract](../contracts/session-boundary.md) A.2; this document states the owner's operations and keeps A.2's wire behaviour.

## Use and scope: responsibility split

CallChannel owns:

- **The call binding.** One entry per registering row instance and connection: the notification names that row's namespace declared, and that connection's unfinished calls by `call_id`. The entry lives until the connection closes, the admitting Rpc load unloads or the row unloads; a call leaves it when its reply is written.
- **The declared-name answer.** A name the registration did not declare is refused before anything is written.
- **The connection's end.** `closed` resolves, and later `notify` answers `gone`, once the connection closes or the Rpc load that admitted the call unloads.
- **Routing the write.** A notification is written by Connection: `write_after` while the call runs, `write` once its reply is written. Progress is a best-effort notice the connection drops when the method did not declare it.

CallChannel writes no socket and holds no connection state.

Callers keep:

- What a notification means: the provider chooses the name and the payload.
- The reply: the callback produces it; Rpc writes it.
- The declarations: the registering part declared them; the table admitted the call with them.
- Interruption: an admitted call is interrupted by the unload of its provider generation or of the Rpc load that admitted it (A.1). A caller's disconnect cancels nothing.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Rpc must run one admitted table call with the caller's notifications enabled | No: the binding, the declared-name answer and the write ordering are the owner's; a `bind`/`finish` pair would make Rpc drive three owners and could leave the binding behind on an error path | `run` |
| A provider must send a notification while or after the call runs, in order after its reply | No: the declared-name check, the binding lookup and the ordering are the owner's | `notify` |
| A provider must report progress under the call's own allowance | No: progress is bounded by the connection's progress budget and the declaration | `progress` |
| A provider must stop when the connection closes or the admitting load unloads | Yes: `closed` composes | `closed` |

An operation or parameter exists only where the row says No.

## Caller example

Rpc runs the call once; the owner binds, invokes, and keeps the entry until the connection, the Rpc load or the row ends:

```rust
let outcome = channel.run(
    ctx,
    RunRequest {
        connection_id: caller.connection_id.clone(),
        call_id: caller.call_id.clone(),
        instance: row_instance.clone(),
        notifications: declared_notifications.clone(),
    },
    Arc::new(move || {
        let table = table.clone();
        let caller = Some(CallerRef {
            connection_id: caller.connection_id.clone(),
            call_id: caller.call_id.clone(),
        });
        let (method, params) = (method.clone(), params.clone());
        Box::pin(async move {
            // The token is the Rpc load's, never the connection's: a disconnect stops delivery only.
            table.call(CallRequest { method, params, caller }, rpc_unload).await
                .map_err(RunError::from)
        })
    }),
)
.await?;
```

A provider inside the callback notifies, reports progress, and waits for the caller to leave:

```rust
match channel.notify(NotifyRequest { connection_id, call_id, method, params }).await {
    Ok(()) => {}
    Err(NotifyError::Undeclared) => return Err(ProviderError::Programming),
    Err(NotifyError::Gone) => return Ok(()), // the caller left; nothing to do
    Err(NotifyError::TooLarge | NotifyError::Unsupported) => return Err(ProviderError::Rejected),
}
channel.progress(ProgressRequest { call_id, data }); // best effort; dropped unless declared
channel.closed(ClosedRequest { connection_id }).await; // also usable after the call returned
```

## Own contract

### Dependencies

Connection's `write_after`, `write` and `closed`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `run` | Run one admitted table call with its caller's notifications enabled | `{connection_id, call_id, instance, notifications}` + `caller: &Ctx` + `call` | `CallOutcome`: the callback's reply or its own error | None; the admitted call is interrupted only by its provider or Rpc unloading (A.1) | `invalid_params` |
| `notify` | Send a notification to the calling connection, after its call's reply when it runs | `{connection_id, call_id, method, params}` | `{}` | None; the frame is queued or refused | `unsupported`, `undeclared`, `gone`, `too_large` |
| `progress` | Report progress for one call | `{call_id, data}` | none | None | none |
| `closed` | Wait until the calling connection is gone | `{connection_id}` | `{}` when the connection closes or the admitting Rpc load unloads; an unknown id resolves at once | The caller's task ends; the wait ends | none |

`run`'s `call` is a callback parameter, not a request field. The owner invokes it once and keeps the entry until the connection closes, the Rpc load unloads or the row unloads; no caller frees the entry on any path. `instance` is the registering row's instance id: two rows of the same language runtime may declare different notifications for the same namespace name.

`progress` is sync and best effort: the connection drops it unless the method declared `progress`. It is the same notice A.2 defines, sent as `management.progress {id, method, data}` for Session and management methods.

### Errors

| Kind | Wire code | What the caller does next |
| --- | --- | --- |
| `invalid_params` | `invalid_params` | The call id is bound to another connection or the request is malformed; a programming error, do not retry |
| `table` | `table` with the table's own code inside | The table refused the call (`service_reloading`, `service_unavailable`, `unknown_method`, `operation_interrupted`); Rpc answers with the table's code |
| `unsupported` | `unsupported` | The notification names a link this binding does not carry; drop the notification, nothing was written |
| `undeclared` | `undeclared` | The registration did not declare the name; fix the declaration, nothing was written. Decided before the connection's state |
| `gone` | `gone` | The connection closed, the Rpc load unloaded or the row unloaded; stop notifying for the call |
| `too_large` | `too_large` | The notification exceeds the connection's frame budget; shorten it — the connection stays open |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `run` | Record the entry for the row and connection (reuse one already bound), invoke the callback, return its outcome | No caller cancellation exists; a disconnect stops delivery only | The row's entries are removed | A connection close or the Rpc load's unload removes the connection's entries; an unloading row answers `gone` | CallChannel: the entry; the caller: the callback's future |
| `notify`, `progress` | Find the entry, check the declared name (before the connection's state), ask Connection to write after the call or directly | — | The entry is gone; `gone` | Pending frames end `gone`; a generation that unloaded answers `gone` for every name | Connection: the frame; CallChannel: the entry |
| `closed` | Wait on Connection's `closed` for the connection, or the Rpc load's unload | The provider stops waiting | The wait ends | The wait ends | Connection: the connection; Rpc: the load |

## Exceptions with reasons

- `run` takes the callback as a direct method parameter (`impl Fn`, not a field or an alias): the callback is never called inside a commit lock, and it may notify and report progress through this owner while it runs. It returns `Result<Outcome, RunError>` with `RunError: From<rutis::host::CallError>`, the form rutis accepts for a callback parameter; a handler's own failure is `MethodError` data inside the outcome, never a callback error code.
- The entry outlives the call because the provider may keep notifying after it returned, as A.2 allows; the connection's close, the Rpc load's unload or the row's unload is the single settlement.

## Supersedes

- [Methods contract](../contracts/session-boundary.md) A.2's Rust binding paragraph (`Caller`, `Callers`, `CallSite`): the operations above replace it; the wire semantics of `progress`, `notify` and `closed` are unchanged.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | CallChannel is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: call bindings belong to process rows and connections. |
| §3 orthogonality | No other owner's state is read; Connection is asked to write and to report its close. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the binding table. |
| §7 one operation | `run` binds and invokes in one call; no `bind` or `finish` is paired or leaked. |
| §7 no proxy | The binding, the declared-name answer and the connection's end are this owner's state; Connection writes the frame. |
| §7 no handle | The binding is addressed by `{connection_id, call_id}`; no writer crosses. |
| ADR 002 D1 | Process scope: calls span rows and connections. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point to Connection as an operation. |
| ADR 002 D4 | No business setting. |
| ADR 002 D5 | The contract is data and one callback; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None. The callback is the form `crates/dim-agent-capabilities/src/host.rs` already gives existing callback parameters; no runtime change is needed.

## Binding appendix

`CallOutcome`, `MethodReply` and `MethodError` are the [methods contract](../contracts/session-boundary.md) A.1 shapes, defined in the [ManagementMethods](ManagementMethods.md) appendix.

```rust
use crate::host::CancelledFn;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub connection_id: String,
    pub call_id: String,
    pub instance: String,
    pub notifications: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyRequest {
    pub connection_id: String,
    pub call_id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressRequest { pub call_id: String, pub data: serde_json::Value }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedRequest { pub connection_id: String }

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RunError {
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("the table refused the call")]
    Table { error: TableError },
}

impl From<TableError> for RunError {
    fn from(error: TableError) -> Self {
        Self::Table { error }
    }
}

impl From<rutis::host::CallError> for RunError {
    fn from(error: rutis::host::CallError) -> Self {
        Self::InvalidParams { message: error.message().to_owned() }
    }
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum NotifyError {
    #[error("The notification needs a link this binding does not carry")]
    Unsupported,
    #[error("The notification is not declared by the registering namespace")]
    Undeclared,
    #[error("The connection is closed or the row unloaded")]
    Gone,
    #[error("The frame exceeds the connection's budget")]
    TooLarge,
}

#[rutis::service(name = "callChannel")]
impl CallChannel {
    pub async fn run(
        &self,
        caller: &Ctx,
        request: RunRequest,
        call: impl Fn() -> BoxFuture<'static, Result<CallOutcome, RunError>> + Send + Sync,
    ) -> Result<CallOutcome, RunError> { /* ... */ }
    pub async fn notify(&self, request: NotifyRequest) -> Result<(), NotifyError> { /* ... */ }
    pub fn progress(&self, request: ProgressRequest) { /* best effort */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
}
```

The native `Caller` type no longer exists: Rust providers keep `{connection_id, call_id}` and call the same operations. `CancelledFn` is not a `run` parameter: the admitted call's interruption comes from its provider or Rpc unloading, never from the connection.
