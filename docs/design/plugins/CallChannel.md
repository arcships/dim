# CallChannel

Scope: process. Issue: #2021.

CallChannel owns the binding between one admitted table call and the connection it arrived on: which notifications the provider may send while the call runs, whether a notification is declared, and when the connection is gone. This document is the owner's target contract; the current implementation is `CallChannelPlugin` in `crates/dim-app-server/src/connection.rs` and `Caller`/`Callers` in `crates/dim-agent-capabilities/src/methods.rs`. Connection, Rpc and the method tables are separate owners.

## Use and scope: responsibility split

CallChannel owns:

- **The call binding.** One per admitted table call: the connection id, the call id, the registering row instance and the notification names that namespace declared. The binding lives for the call's extent — from `run` until the callback ends or the caller's future is dropped — and the owner always removes it.
- **The declared-name answer.** A name the registration did not declare is refused before anything is written.
- **The connection's end.** `closed` resolves when the connection closes; the binding does not have to be alive.
- **Routing the write.** A notification is written by Connection, addressed by the call id so it follows the reply.

CallChannel writes no socket and holds no connection state.

Callers keep:

- What a notification means: the provider chooses the name and the payload.
- The reply: the callback produces it; Rpc writes it after `run` returns.
- The declarations: the registering part declared them; the table admitted the call with them.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Rpc must run one admitted table call with the caller's notifications enabled and its reply ordered | No: the binding, the declared-name answer and the write ordering are the owner's; a `bind`/`finish` pair would make Rpc drive three owners and could leave the binding behind on an error path | `run` |
| A provider must send a notification to the calling connection, in order after its reply | No: the declared-name check, the binding lookup and the ordering are the owner's | `notify` |
| A provider must stop when the connection closes, including after the call returned | Yes: `closed` composes, and it is addressed by the connection alone | `closed` |

An operation or parameter exists only where the row says No.

## Caller example

Rpc runs the call once; the owner binds, invokes and settles:

```rust
let reply = channel.run(RunRequest {
    connection_id: caller.connection_id.clone(),
    call_id: caller.call_id.clone(),
    instance: row_instance.clone(),
    notifications: declared_notifications.clone(),
    call: Arc::new(move || {
        let table = table.clone();
        let (method, params) = (method.clone(), params.clone());
        Box::pin(async move { table.call(CallRequest { method, params, caller: None }).await })
    }),
}, cancel).await?;
// Notifications sent inside the callback were held by Connection; the reply releases them in order.
```

A provider inside the callback builds a notification and waits for the caller to leave:

```rust
match channel.notify(NotifyRequest { connection_id, call_id, method, params }).await {
    Ok(()) => {}
    Err(NotifyError::Undeclared) => return Err(ProviderError::Programming),
    Err(NotifyError::Gone) => return Ok(()), // the caller left; nothing to do
    Err(NotifyError::TooLarge) => return Err(ProviderError::TooLarge),
}
channel.closed(ClosedRequest { connection_id }).await; // also usable after the call returned
```

## Own contract

### Dependencies

Connection's `write_after` and `closed`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `run` | Run one admitted table call with its caller's notifications enabled | `{connection_id, call_id, instance, notifications, call}` | the callback's reply | The caller's token: the callback stops and the binding is removed | the callback's own error; `duplicate` |
| `notify` | Send a notification to the calling connection while the call runs | `{connection_id, call_id, method, params}` | `{}` | None; the frame is queued or refused | `undeclared`, `gone`, `too_large` |
| `closed` | Wait until the calling connection is gone | `{connection_id}` | `{}` when the connection closes; an unknown id resolves at once | The caller's task ends; the wait ends | none |

`run`'s `call` is a callback by reference. The owner invokes it once with the binding open and removes the binding when the callback ends or the caller's future is dropped; no caller frees the binding on any path. `instance` is the registering row's instance id: two rows of the same language runtime may declare different notifications for the same namespace name.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate` | The call id is bound already; this is a programming error in the caller |
| `undeclared` | The registration did not declare the name; fix the declaration, nothing was written |
| `gone` | The call ended or the connection closed; stop notifying for the call |
| `too_large` | The notification exceeds the connection's frame budget; shorten it — the connection stays open |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `run` | Record the binding under the row instance and the connection, invoke the callback, remove the binding | The callback stops; the binding is removed; nothing else changes | The caller's future drops; a guard removes the binding | A connection close interrupts the call through its cancellation; the binding is removed | CallChannel: the binding; the caller: the callback's future |
| `notify` | Find the binding, check the declared name, ask Connection to write after the call | The provider's task ends before the write; nothing is written | The binding is gone; `gone` | Pending notifications are `gone` | Connection: the frame; CallChannel: the binding |
| `closed` | Wait on Connection's `closed` for the connection | The provider stops waiting | The wait ends | The wait ends | Connection |

## Exceptions with reasons

- `run` takes the callback by reference, like SessionScope's `withHold`: the callback is never called inside a commit lock, and it may notify through this owner while it runs.
- A notification sent after the callback returned is `gone`: the binding is the call's, not the connection's, so a provider that wants to react to the caller leaving uses `closed`, which is addressed by the connection and needs no binding.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | CallChannel is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: call bindings belong to process rows and connections. |
| §3 orthogonality | No other owner's state is read; Connection is asked to write and to report its close. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the binding table. |
| §7 one operation | `run` binds, invokes and settles in one call; there is no `bind` or `finish` to pair or leak. |
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
pub type CallFn = Arc<
    dyn Fn() -> BoxFuture<'static, Result<serde_json::Value, CallError>> + Send + Sync,
>;

#[derive(Clone)]
pub struct RunRequest {
    pub connection_id: String,
    pub call_id: String,
    pub instance: String,
    pub notifications: BTreeSet<String>,
    pub call: CallFn,
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
pub struct ClosedRequest { pub connection_id: String }

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyError { Undeclared, Gone, TooLarge }

#[rutis::service(name = "callChannel")]
impl CallChannel {
    pub async fn run(&self, request: RunRequest, cancel: CancellationToken)
        -> Result<serde_json::Value, CallError> { /* ... */ }
    pub async fn notify(&self, request: NotifyRequest) -> Result<(), NotifyError> { /* ... */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
}
```

`CallFn` is passed by reference; the owner invokes it once and removes the binding when it returns or the caller's future is dropped. The native `Caller` type no longer exists: Rust providers keep `{connection_id, call_id}` and call the same two operations.
