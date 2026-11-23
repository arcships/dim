# Rpc

Scope: process. Issue: #2021.

Rpc owns the daemon's inbound protocol entry: the handshake, the built-in methods, the routing to method namespaces, and the settlement of accepted commands. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/connection.rs` (`RpcService`, `RpcPlugin`). Connection, ManagementMethods, CallChannel, Agents and ServerControl are separate owners with their own contracts.

## Use and scope: responsibility split

Rpc owns:

- **The handshake.** Which protocol versions a connection may greet with, what the greeting records, and what the greeting reply carries.
- **Built-in methods.** `daemon.ping`, `daemon.shutdown`, `session.*` and the other methods the protocol reserves; each built-in's admission and reply.
- **Routing.** For each call: the management table, the Session's table, or a built-in; the caller identity the table receives; the call id notifications are addressed by.
- **Command settlement.** The commands a call starts, their drain on unload, and the interruption kinds their declarations name.

Rpc writes no Timeline fact and holds no connection.

Callers keep:

- Reading frames and writing replies: Connection owns the socket, the outbound queue and the reply ordering.
- The namespace table and the registered handlers: ManagementMethods and the Session's method table own them.
- Authorizing Session access: Agents decides whether a Session may be acquired; the built-in methods carry the decision.
- The shutdown sequence: ServerControl owns the intent; the assembly runs the sequence.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A greeting must be answered or refused with the current generation | No: the version window, the records and the method list are the owner's state | `hello` |
| A frame must be dispatched by the protocol rules | No: built-ins, routing and the caller identity are the owner's | `request` |
| A call's commands must drain when this generation unloads | No: the command set is the owner's | `request` (settlement) |

An operation or parameter exists only where the row says No.

## Caller example

```rust
// Connection reads one frame and asks the current generation.
let hello = rpc.hello(HelloRequest { connection_id: id.clone(), params }).await?;
connection.write(hello.reply);

let reply = rpc.request(RequestCall { connection_id: id, call_id, request }).await?;
connection.write_after(WriteAfter { connection_id: id, call_id, frame: reply.frame }).await?;
```

## Own contract

### Dependencies

Connection's `write_after` and `attached`; ManagementMethods' `call`; the Session's method table (the same shape, per [ManagementMethods](ManagementMethods.md)); Agents' `acquire` and running/close operations; CallChannel's `bind` and `finish`; ServerControl's `shutdown`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `hello` | Answer a client's greeting for this generation | `{connection_id, params}` | `{reply, records}` | None; the handshake is one step | `reloading` (no generation is serving), `invalid` (version, entry or hello fields), `closed` |
| `request` | Dispatch one call and settle what it starts | `{connection_id, call_id, request}` | `{reply}` | The calling connection closing ends the call as interrupted; an unload ends it by the method's declaration | `reloading`, `unknown_method`, `interrupted`, `invalid`, and the method's own error |

`request` mints nothing: the `call_id` comes from Connection, so the provider, CallChannel and the reply ordering name one call. The method's own errors are the data the table returns; Rpc maps them to the protocol envelope and does not add kinds.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `reloading` | Retry after the next generation is installed; the connection stays open |
| `unknown_method` | The name no namespace holds; the client sees the method error |
| `interrupted` | The call may have committed; read the current state before retrying |
| `invalid` | The frame or the greeting is malformed; answer the client with the protocol error |
| `closed` | The connection is gone; nothing was dispatched |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `hello` | Check the version window and the entry, record the greeting, build the method list | — | — | New hellos are refused as `reloading`; open connections stay | Rpc |
| `request` | Route, admit through the table with the caller identity and `call_id`, return the reply; commands settle in Rpc's tracker | A closing connection ends the call as interrupted | The call ends and its commands settle | An unloading generation cancels its commands, answers pending waits as interrupted and drains before unload; connections stay open | Rpc: the commands |

## Exceptions with reasons

- The built-in methods are a closed set on the owner type, not registrations: they are the protocol's own methods and never appear in the management table.

## Rule check

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

- **Session registration (#2026)**: the Session method table's dispatch takes the same `{method, params, caller: {connectionId, callId}}` request and returns the same reply data, so one call path serves both tables.

## Binding appendix

```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloRequest { pub connection_id: String, pub params: HelloParams }

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloReply { pub reply: Frame, pub records: Vec<String> }

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCall {
    pub connection_id: String,
    pub call_id: String,
    pub request: Request,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestReply { pub reply: Reply }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RpcError {
    #[error("Service is reloading; retry")]
    Reloading,
    #[error("Unknown method")]
    UnknownMethod,
    #[error("Service reloaded; the call may have committed")]
    Interrupted,
    #[error("{message}")]
    Invalid { message: String },
    #[error("The connection is closed")]
    Closed,
}

#[rutis::service(name = "rpc")]
impl RpcService {
    pub async fn hello(&self, request: HelloRequest) -> Result<HelloReply, RpcError> { /* ... */ }
    pub async fn request(&self, request: RequestCall) -> Result<RequestReply, RpcError> { /* ... */ }
}
```

`Connection` looks the `rpc` service up on each frame; a generation that is unloading refuses new frames as `reloading`.
