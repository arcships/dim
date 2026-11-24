# Rpc

Scope: process. Issue: #2021.

Rpc owns the daemon's inbound protocol entry: the handshake, the built-in methods, the routing to method namespaces, and the settlement of accepted commands. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/connection.rs` (`RpcService`, `RpcPlugin`). Connection, ManagementMethods, CallChannel, Agents and ServerControl are separate owners with their own contracts. The wire behaviour of methods — declarations, interruptions, `callChannel` — is the [methods contract](../contracts/session-boundary.md); this document states the owner's operations.

## Use and scope: responsibility split

Rpc owns:

- **The handshake.** Which protocol versions a connection may greet with, what the greeting records, and the method list the greeting carries.
- **Built-in methods.** `daemon.ping`, `daemon.shutdown`, `session.*` and the other methods the protocol reserves; each built-in's admission and reply.
- **Routing.** For each call: the management table, the Session's table, or a built-in; the caller identity the table receives; the call id notifications are addressed by.
- **Command settlement.** The commands a call starts, their drain on unload, and the interruption kinds their declarations name.

Rpc writes no Timeline fact and holds no connection.

Callers keep:

- Reading frames and writing replies: Connection owns the socket, the outbound queue and the reply ordering.
- The namespace table and the registered handlers: ManagementMethods and the Session's method table own them.
- Authorizing Session access: Agents decides whether a Session may be acquired.
- The shutdown sequence: ServerControl owns the intent; the assembly runs the sequence.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A greeting must be answered or refused with the current generation | No: the version window, the records and the method list are the owner's state | `hello` |
| A frame must be dispatched by the protocol rules | No: built-ins, routing and the caller identity are the owner's | `request` |
| A call's commands must drain when this generation unloads | No: the command set is the owner's | `request` (settlement) |

An operation or parameter exists only where the row says No.

## Caller example

Connection reads one frame and asks the current generation:

```rust
let hello = rpc.hello(HelloRequest { connection_id: id.clone(), params }, cancelled.clone()).await?;
connection.reply(ReplyRequest { connection_id: id.clone(), call_id, frame: hello.reply }).await?;

let reply = rpc.request(RequestCall { connection_id: id, call_id, request }, cancelled).await?;
connection.reply(ReplyRequest { connection_id: id, call_id, frame: reply.reply }).await?;
```

## Own contract

### Dependencies

Connection's `reply` and `attached`; ManagementMethods' `call`; the Session's method table by its own contract ([SessionMethods, PR #2103](https://github.com/arcships/dim/pull/2103)); Agents' `acquire` and `sessions` ([Agents contract, PR #2098](https://github.com/arcships/dim/pull/2098)); CallChannel's `run`; ServerControl's `shutdown`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `hello` | Answer a client's greeting for this generation | `{connection_id, params}` | `{reply, instance, methods}`: the frame, this generation's instance id and the management method names | The caller's token; the handshake stops before anything is recorded | `service_reloading`, `invalid_params`, `service_unavailable` |
| `request` | Dispatch one call and settle what it starts | `{connection_id, call_id, request}` | `{reply}`: the frame, built from the table's `CallOutcome` | The calling load's token (A.1); the provider's call is not cancelled by the connection closing (see below) | `service_reloading`, `service_unavailable`, `unknown_method`, `operation_interrupted`, `invalid_params`, and the method's own error |

`request` mints nothing: the `call_id` comes from Connection, so the provider, CallChannel and the reply ordering name one call. The method's own error is the data the table returns; Rpc maps it to the protocol envelope and does not add kinds.

A calling connection closing stops delivery only: the admitted call belongs to the provider generation and settles according to its declaration, as the [methods contract A.1 and A.2](../contracts/session-boundary.md) require. The token `request` receives is the admitting Rpc load's, never the connection's. Rpc's `request` returns the reply frame; writing it is Connection's.

An idle close reads Agents' `sessions` value and calls `connection.close_for_idle`; on `Busy` it answers `daemon_busy` with the three counts. After the refusal is committed the assembly quiesces Agents and settles. Agents' `quiesce` answer is [#2098](https://github.com/arcships/dim/pull/2098)'s; at this call site it runs after the commit, so a `Busy { running }` answer is settled by the graceful close and cannot reopen admission — Agents' pause is one-way and cannot be reverted.

### Errors

The wire codes are the existing ones from the methods contract; a client matches them unchanged.

| Kind | Wire code | What the caller does next |
| --- | --- | --- |
| `service_reloading` | `service_reloading` | Retry after the next generation is installed; the connection stays open |
| `service_unavailable` | `service_unavailable` | The generation cannot run; reconnect or report the service as unavailable |
| `unknown_method` | `unknown_method` | The name no namespace holds; the client sees the method error |
| `operation_interrupted` | `operation_interrupted` | The call may have committed; read the current state before retrying |
| `invalid_params` | `invalid_params` | The frame or the greeting is malformed; answer the client with the protocol error |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `hello` | Check the version window and the entry, record the greeting, build the method list | — | — | New hellos are refused as `service_reloading`; open connections stay | Rpc |
| `request` | Route, admit through the table with the caller identity and `call_id`, return the reply; commands settle in Rpc's tracker | A closing connection stops waiting for the frame; the accepted call continues and settles by its declaration | The call finishes and its commands settle | An unloading generation cancels its commands, answers pending waits as `operation_interrupted` and drains before unload; connections stay open | Rpc: the commands |

## Exceptions with reasons

- The built-in methods are a closed set on the owner type, not registrations: they are the protocol's own methods and never appear in the management table.

## Supersedes

None. The method declarations, interruptions and `callChannel` behaviour stay with the [methods contract](../contracts/session-boundary.md); this contract states the dispatcher owner's operations.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Rpc is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: the dispatcher serves every connection and every Session. |
| §3 orthogonality | No other owner's state is read; Connection, the tables, CallChannel and Agents are asked through their operations. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use `hello` and `request`; no caller sees the command set or the routing tables. |
| §8 rule | An unloading generation settles its accepted commands before its services go. |
| §7 no proxy | The reply is returned as data and written by Connection; Rpc does not hold the socket. |
| ADR 002 D1 | Process scope: one dispatcher per process generation. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point to the owners being asked; none is stored. |
| ADR 002 D4 | No business setting. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Session registration (#2026)**: none. The Session table's shape is [#2103](https://github.com/arcships/dim/pull/2103)'s latest head; Rpc calls each table by its own contract.

## Binding appendix

```rust
use crate::host::CancelledFn;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloRequest { pub connection_id: String, pub params: HelloParams }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloReply {
    pub reply: Frame,
    pub instance: String,
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCall {
    pub connection_id: String,
    pub call_id: String,
    pub request: Request,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestReply { pub reply: Frame }

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RpcError {
    #[error("Service is reloading; retry")]
    ServiceReloading,
    #[error("Service is unavailable until it is fixed or restarted")]
    ServiceUnavailable,
    #[error("Unknown method")]
    UnknownMethod,
    #[error("Service reloaded; the call may have committed")]
    OperationInterrupted,
    #[error("{message}")]
    InvalidParams { message: String },
}

#[rutis::service(name = "rpc")]
impl RpcService {
    pub async fn hello(&self, request: HelloRequest, cancelled: CancelledFn)
        -> Result<HelloReply, RpcError> { /* ... */ }
    pub async fn request(&self, request: RequestCall, cancelled: CancelledFn)
        -> Result<RequestReply, RpcError> { /* ... */ }
}
```

`Frame` is the encoded protocol frame the client receives; `Request` is the decoded call. Connection looks the `rpc` service up on each frame; a generation that is unloading refuses new frames as `service_reloading`.
