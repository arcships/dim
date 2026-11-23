# CallChannel

Scope: process. Issue: #2021.

CallChannel owns the binding between an admitted table call and the connection it arrived on: which notifications the provider may send, whether a notification is declared, and when the connection is gone. This document is the owner's target contract; the current implementation is `CallChannelPlugin` in `crates/dim-app-server/src/connection.rs` and `Caller`/`Callers` in `crates/dim-agent-capabilities/src/methods.rs`. Connection, Rpc and the method tables are separate owners. The notification, progress and closed semantics are the [methods contract](../contracts/session-boundary.md) A.2; this document states the owner's operations.

## Use and scope: responsibility split

CallChannel owns:

- **The call binding.** One per admitted table call: the connection id, the call id, the registering row instance and the notification names that namespace declared. The binding lives until the connection closes or the registering row unloads; a notification sent after the call returned still finds it.
- **The declared-name answer.** A name the registration did not declare is refused before anything is written.
- **The connection's end.** `closed` resolves when the binding's connection closes.
- **Routing the write.** A notification is written by Connection: `write_after` while the call runs, `write` after it returned. Progress is written under the call's own allowance.

CallChannel writes no socket and holds no connection state.

Callers keep:

- What a notification means: the provider chooses the name and the payload.
- The reply: the callback produces it; Rpc writes it.
- The declarations: the registering part declared them; the table admitted the call with them.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Rpc must run one admitted table call with the caller's notifications enabled | No: the binding, the declared-name answer and the write ordering are the owner's; a `bind`/`finish` pair would make Rpc drive three owners and could leave the binding behind on an error path | `run` |
| A provider must send a notification while or after the call runs, in order after its reply | No: the declared-name check, the binding lookup and the ordering are the owner's | `notify` |
| A provider must report progress under the call's own allowance | No: progress is bounded by the connection's progress budget and the declaration | `progress` |
| A provider must stop when the connection closes | Yes: `closed` composes | `closed` |

An operation or parameter exists only where the row says No.

## Caller example

Rpc runs the call once; the owner binds, invokes, and leaves the binding until the connection closes:

```rust
let reply = channel.run(
    ctx,
    RunRequest {
        connection_id: caller.connection_id.clone(),
        call_id: caller.call_id.clone(),
        instance: row_instance.clone(),
        notifications: declared_notifications.clone(),
    },
    Arc::new(move || {
        let table = table.clone();
        let (method, params) = (method.clone(), params.clone());
        Box::pin(async move { table.call(CallRequest { method, params, caller: None }, cancelled).await })
    }),
    cancelled,
)
.await?;
// The provider may notify after this returned; the binding ends with the connection or the row.
```

A provider inside the callback notifies, reports progress, and waits for the caller to leave:

```rust
match channel.notify(NotifyRequest { connection_id, call_id, method, params }).await {
    Ok(()) => {}
    Err(NotifyError::Undeclared) => return Err(ProviderError::Programming),
    Err(NotifyError::Gone) => return Ok(()), // the caller left; nothing to do
    Err(NotifyError::TooLarge) => return Err(ProviderError::TooLarge),
}
channel.progress(ProgressRequest { connection_id, call_id, data }).await;
channel.closed(ClosedRequest { connection_id }).await; // also usable after the call returned
```

## Own contract

### Dependencies

Connection's `write_after`, `write` and `closed`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `run` | Run one admitted table call with its caller's notifications enabled | `{connection_id, call_id, instance, notifications}` + `caller: &Ctx` + `call` | the callback's `MethodReply` | The caller's token stops the callback; the binding stays with the connection | `invalid_params`, `callback` |
| `notify` | Send a notification to the calling connection, after its call's reply when it runs | `{connection_id, call_id, method, params}` | `{}` | None; the frame is queued or refused | `undeclared`, `gone`, `too_large` |
| `progress` | Report progress for one call | `{connection_id, call_id, data}` | `{}` | None | `undeclared`, `gone`, `too_large` |
| `closed` | Wait until the calling connection is gone | `{connection_id}` | `{}` when it closes; an unknown id resolves at once | The caller's task ends; the wait ends | none |

`run`'s `call` is a callback parameter, not a request field. The owner invokes it once and keeps the binding until the connection closes or the registering row unloads; no caller frees the binding on any path. `instance` is the registering row's instance id: two rows of the same language runtime may declare different notifications for the same namespace name.

### Errors

| Kind | Wire code | What the caller does next |
| --- | --- | --- |
| `invalid_params` | `invalid_params` | The call id is bound to another connection or the request is malformed; a programming error |
| `undeclared` | `undeclared` | The registration did not declare the name; fix the declaration, nothing was written |
| `gone` | `gone` | The connection closed or the row unloaded; stop notifying for the call |
| `too_large` | `too_large` | The frame exceeds the connection's budget (a notification) or progress allowance; shorten it — the connection stays open |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `run` | Record the binding under the row instance and the connection (reuse one already bound for the same row and connection), invoke the callback, return its reply | The callback stops; the binding stays with the connection | The row's bindings are removed | A connection close removes every binding of that connection | CallChannel: the binding; the caller: the callback's future |
| `notify`, `progress` | Find the binding, check the declared name, ask Connection to write after the call (or directly, once the reply is written) | The provider's task ends; nothing is written | The binding is gone; `gone` | Pending frames end `gone` | Connection: the frame; CallChannel: the binding |
| `closed` | Wait on Connection's `closed` for the connection | The provider stops waiting | The wait ends | The wait ends | Connection |

## Exceptions with reasons

- `run` takes the callback by reference, like SessionScope's `withHold`: the callback is never called inside a commit lock, and it may notify and report progress through this owner while it runs.
- The binding outlives the call because the provider may keep notifying after it returned, as A.2 allows; the connection's close or the row's unload is the single settlement.

## Supersedes

- [Methods contract](../contracts/session-boundary.md) A.2's Rust binding paragraph (`Caller`, `Callers`, `CallSite`): the operations above replace it; the wire semantics of `notify`, `progress` and `closed` are unchanged.

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

None. The callback is the same binding rutis already gives `withHold` and FileStorage's `write` source; no runtime change is needed.

## Binding appendix

```rust
use crate::host::CancelledFn;
use rutis::Ctx;

pub type CallFn = Arc<
    dyn Fn() -> BoxFuture<'static, Result<MethodReply, MethodError>> + Send + Sync,
>;

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
pub struct ProgressRequest {
    pub connection_id: String,
    pub call_id: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedRequest { pub connection_id: String }

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RunError {
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("{code}: {message}")]
    Callback { code: String, message: String },
}

impl From<MethodError> for RunError {
    fn from(error: MethodError) -> Self {
        Self::Callback { code: error.code, message: error.message }
    }
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum NotifyError {
    #[error("The notification is not declared by the registering namespace")]
    Undeclared,
    #[error("The connection is closed or the row unloaded")]
    Gone,
    #[error("The frame exceeds the connection's budget")]
    TooLarge,
}

#[rutis::service(name = "callChannel")]
impl CallChannel {
    pub async fn run(&self, caller: &Ctx, request: RunRequest, call: CallFn, cancelled: CancelledFn)
        -> Result<MethodReply, RunError> { /* ... */ }
    pub async fn notify(&self, request: NotifyRequest) -> Result<(), NotifyError> { /* ... */ }
    pub async fn progress(&self, request: ProgressRequest) -> Result<(), NotifyError> { /* ... */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
}
```

`MethodReply` and `MethodError` are the [methods contract](../contracts/session-boundary.md) A.1 shapes; `MethodError` is `{code, message}`, defined in the [ManagementMethods](ManagementMethods.md) appendix. The native `Caller` type no longer exists: Rust providers keep `{connection_id, call_id}` and call the same operations.
