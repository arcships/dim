# CallChannel

Scope: process. Issue: #2021.

CallChannel owns the binding between one admitted call and the connection it arrived on: which notifications the provider may send, whether the call is still answerable, and when the connection is gone. This document is the owner's target contract; the current implementation is `CallChannelPlugin` in `crates/dim-app-server/src/connection.rs` and `Caller`/`Callers` in `crates/dim-agent-capabilities/src/methods.rs`. Connection, Rpc and the method tables are separate owners.

## Use and scope: responsibility split

CallChannel owns:

- **Bindings.** For each admitted call that may notify, the connection id, the call id, the row instance that registered the namespace, and the notification names that namespace declared.
- **The declared-name answer.** A name the registration did not declare is refused before anything is written.
- **The connection's end.** A binding resolves when its connection closes, when the reply is written, or when the registering row unloads.
- **Routing the write.** A notification is written by Connection, addressed by the call id so it follows the reply.

CallChannel writes no socket and holds no connection state.

Callers keep:

- What a notification means: the provider chooses the name and the payload.
- The reply: Rpc returns it; Connection writes it.
- The declarations: the registering part declared them; the table admitted the call with them.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Rpc must make an admitted call addressable before the handler can notify | No: the binding's lifetime and the declared-name answer are the owner's | `bind` |
| A provider must send a notification to the calling connection, in order after its reply | No: the declared-name check, the binding lookup and the ordering are the owner's | `notify` |
| A provider must stop when the connection closes | Yes: `closed` composes | `closed` |
| Rpc must end a binding once the reply is written | No: the end and the removal are the owner's | `finish` |

An operation or parameter exists only where the row says No.

## Caller example

Rpc binds the call before invoking the handler:

```rust
channel.bind(BindRequest {
    connection_id: caller.connection_id.clone(),
    call_id: caller.call_id.clone(),
    instance: row_instance.clone(),
    notifications: declarations.iter().flat_map(|d| d.notifications.clone()).collect(),
}).await?;
let reply = table.call(CallRequest { method, params, caller: Some(caller.clone()) }).await?;
connection.write_after(WriteAfter { connection_id, call_id, frame: reply.frame }).await?;
channel.finish(FinishRequest { connection_id, call_id }).await?;
```

A provider notifies and waits for the end:

```rust
match channel.notify(NotifyRequest { connection_id, call_id, method, params }).await {
    Ok(()) => {}
    Err(NotifyError::Undeclared) => return Err(ProviderError::Programming),
    Err(NotifyError::Gone) => return Ok(()), // the caller left; nothing to do
    Err(NotifyError::TooLarge) => return Err(ProviderError::TooLarge),
}
channel.closed(ClosedRequest { connection_id, call_id }).await;
```

## Own contract

### Dependencies

Connection's `write_after` and `closed`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `bind` | Make one admitted call addressable by its notifications | `{connection_id, call_id, instance, notifications}` | `{}` | None | `duplicate` |
| `notify` | Send a notification to the calling connection, after the call's reply | `{connection_id, call_id, method, params}` | `{}` | None; the frame is queued or refused | `undeclared`, `gone`, `too_large` |
| `closed` | Wait until the calling connection is gone | `{connection_id, call_id}` | `{}` when the connection closes | The provider's task ends; the wait ends | none |
| `finish` | The call's reply was written; the binding ends | `{connection_id, call_id}` | `{}` | None | none |

`instance` is the registering row's instance id: two rows of the same language runtime may declare different notifications for the same namespace name, and the binding belongs to the row that admitted the call.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate` | The call id is bound already; this is a programming error in the caller |
| `undeclared` | The registration did not declare the name; fix the declaration, nothing was written |
| `gone` | The connection closed or the row unloaded; stop notifying for this call |
| `too_large` | The notification exceeds the connection's frame budget; shorten it — the connection stays open |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `bind` | Record the binding under the row instance and the connection | — | The row's bindings are removed | New bindings are refused; existing ones resolve as gone | CallChannel |
| `notify` | Find the binding, check the declared name, ask Connection to write after the call | The provider's task ends before the write; nothing is written | The binding is removed; `gone` | Pending notifications are `gone` | Connection: the frame; CallChannel: the binding |
| `closed` | Wait on Connection's `closed` for the binding's connection | The provider stops waiting | The wait ends | The wait ends | Connection |
| `finish` | Remove the binding; later notifications are `gone` | — | Already removed | Already removed | CallChannel |

## Exceptions with reasons

- `bind`/`finish` are called by Rpc, not by the table: the table is in another crate and must not depend on the App Server; Rpc knows both the admitted call and the connection.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | CallChannel is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: call bindings belong to process rows and connections. |
| §3 orthogonality | No other owner's state is read; Connection is asked to write and to report its close. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the binding table. |
| §7 no proxy | The binding, the declared-name answer and the connection's end are this owner's state; Connection writes the frame. |
| §7 no handle | The binding is addressed by `{connection_id, call_id}`; no writer crosses. |
| ADR 002 D1 | Process scope: calls span rows and connections. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point to Connection as an operation. |
| ADR 002 D4 | No business setting. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None. The call id is data minted by Rpc; no runtime change is needed.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindRequest {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyError { Undeclared, Gone, TooLarge }

#[rutis::service(name = "callChannel")]
impl CallChannel {
    pub async fn bind(&self, request: BindRequest) -> Result<(), BindError> { /* ... */ }
    pub async fn notify(&self, request: NotifyRequest) -> Result<(), NotifyError> { /* ... */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
    pub async fn finish(&self, request: FinishRequest) { /* ... */ }
}
```

The native `Caller` type no longer exists: Rust providers keep `{connection_id, call_id}` and call the same two operations.
