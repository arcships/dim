# Transport

Scope: process. Issue: #2021.

Transport owns the daemon's listening endpoints. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/transport.rs`. Connection, ServerControl and Rpc are separate owners with their own contracts; their calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

Transport owns:

- **Endpoints.** The Unix socket, the named pipe and the TCP listener, their files, their private directory and their lock. An endpoint exists only because Transport bound it.
- **Binding order.** Endpoints bind after the database lock is held, when the host has one, and accepting starts only after the host publishes.
- **Accepting.** The accept loop that turns a listener event into an accepted stream, and the stop when the daemon closes.
- **Endpoint faults.** The first listener exit is reported once as a process fault.

Callers keep:

- What a connection is and what happens to its frames: Connection owns admitted streams.
- The daemon's shutdown sequence: ServerControl owns the intent; the assembly runs the sequence.
- How a launcher learns the address: the value `read` returns; the CLI's startup output is a launcher's transport.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A launcher or an entry plugin needs the addresses actually bound | No: the bound port after a bind to zero and a non-UTF-8 socket path are the owner's knowledge | `read` |

An operation or parameter exists only where the row says No.

## Caller example

```rust
let bound = transport.read().await?;
if let Some(path) = bound.unix_path {
    connect(path).await?; // the caller decides which endpoint it uses
}
```

## Own contract

### Dependencies

Connection's `accept`, called once per accepted stream; ServerControl's `activated` and `fault`. Neither is stored.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `read` | The addresses this process bound | `{}` | `{unixPath?, pipeName?, websocketAddress?}` | None | `invalid_path` |

`read` converts the bound addresses to UTF-8 text; a Unix path that is not UTF-8 is `invalid_path`, and the rest of the reply is not sent.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_path` | The bound Unix path is not UTF-8; use another endpoint or start the daemon with a UTF-8 path |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `read` | Read the owner's bound values | — | — | Pending calls end with the generation | Transport |
| (accept loop) | Wait for `activated`, accept, call `Connection::accept`, repeat | — | The listener task ends with the owner | The loop stops on `closing`; the socket file is removed only when its inode is still the one this generation created | Transport: the listener and its file |

## Exceptions with reasons

- The accept loop is internal: a listener event is not data. Transport calls `Connection::accept` crate-internally; the stream's lifecycle is fixed in the [Connection contract](Connection.md).

## Supersedes

- [Service faces](../service-faces.md) §4.9's `listenAddresses.read` line: the operation is the one in this contract.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.


| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Transport is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: endpoints outlive every Session. |
| §3 orthogonality | No other owner's state is read; Connection and ServerControl are asked through their operations. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use `read`; no caller sees a listener or a socket file. |
| §4 rule 8 | No infrastructure change. |
| §7 object handover | Only the accepted stream, by `Connection::accept`; fixed in the Connection contract. |
| ADR 002 D1 | Process scope: endpoints are process resources. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Transport depends on Connection and ServerControl as operations. |
| ADR 002 D4 | Endpoints are structural configuration from the loader row; no business setting. |
| ADR 002 D5 | `read` is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundAddresses {
    pub unix_path: Option<String>,
    pub pipe_name: Option<String>,
    pub websocket_address: Option<String>,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ListenError {
    #[error("{message}")]
    InvalidPath { message: String },
}

#[rutis::service(name = "listenAddresses")]
impl Transport {
    pub async fn read(&self, request: Empty) -> Result<BoundAddresses, ListenError> { /* ... */ }
}
```
