# ManagementMethods

Scope: process. Issue: #2021.

ManagementMethods owns the process namespace table: which namespaces a process part registered, what each declared, and who answers a name now. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/methods.rs` and `crates/dim-agent-capabilities/src/methods.rs`. Rpc, the Session method table ([#2026](../contracts/session-boundary.md)) and the registering parts are separate owners.

## Use and scope: responsibility split

ManagementMethods owns:

- **Registrations.** One namespace per registering row, its declarations and its call handler; a registration ends when its row unloads.
- **Admission.** One call: the name's owner, the interruption kind its declaration names, the caller identity, and the settlement of accepted calls on unload.
- **Withdrawn namespaces.** The last row that held each namespace, so a call after a withdrawal answers from that row's status instead of leaking to a parent namespace.
- **Reserved names.** The host's own namespaces and built-ins are refused at registration.

Callers keep:

- What the methods mean: each registering part declares and implements its namespaces.
- The client methods themselves: Rpc serves them; this table only dispatches.
- When to call: Rpc decides routing; the table answers one name.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A process part publishes its namespace for the row's lifetime | No: the registration's lifetime and the duplicate check are the owner's | `register` |
| Rpc must dispatch one call to the namespace that holds the name now | No: the lookup, the caller identity, the interruption and the withdrawn-row answer are the owner's | `call` |

An operation or parameter exists only where the row says No.

## Caller example

A process part registers from `apply`:

```rust
table.register(RegisterRequest {
    namespace: "settings".into(),
    declarations,
    call: handler, // a callback by reference; never called inside a commit lock
}).await?;
```

Rpc dispatches:

```rust
// `reloading` and `unavailable` are answered from the registering row's status.
let reply = table.call(CallRequest {
    method: method.into(),
    params,
    caller: Some(CallerRef { connection_id, call_id }),
}).await?;
```

## Own contract

### Dependencies

The loader's row status (`rutis/loader`), read when a withdrawn namespace answers; the registering rows' `ctx` for the registration lifetime.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a namespace for the registering row's lifetime | `{namespace, declarations, call}` | `{}` | None; the owner completes it | `reserved`, `duplicate`, `invalid` |
| `call` | Dispatch one call to the namespace that holds it | `{method, params, caller?}` | `{reply}` | The calling connection closing ends the call as interrupted; an unload ends it by its declaration (`report` or `drain`) | `unknown_method`, `reloading`, `unavailable`, `interrupted`, `invalid` |

`caller` is `{connectionId, callId}`: the connection the call arrived on and the call id the reply ordering uses. It is absent for an in-process call. The table passes it to the handler unchanged; a handler that notifies uses it with `callChannel` or the native binding.

A call's reply is `MethodReply` data: a value, or the handler's own error with its `code` and `message`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `reserved` | The namespace or a name is the host's; pick another name |
| `duplicate` | The namespace is registered already; the registering row must not register twice |
| `invalid` | The namespace or declaration is outside the name grammar |
| `unknown_method` | No registered or withdrawn namespace holds the name; the client sees an unknown method |
| `reloading` | The registering row is starting or reloading; retry after it is Active |
| `unavailable` | The registering row failed or cannot run; the client sees the service as unavailable |
| `interrupted` | The provider unloaded while the call ran; the call may have committed, so read the current state before retrying |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the name and declarations, record them under the row, refuse a duplicate | — | The registration is withdrawn and the row recorded | New registrations are refused; the table reloads with the process | The registering row |
| `call` | Find the owner, admit the call, run the handler, return the reply; on unload, interrupt `report` and wait for `drain` | A closing connection ends the call as interrupted | The call ends and its commands settle | A withdrawn namespace answers from its row's status | The table for the call's settlement; the handler for its effects |

## Exceptions with reasons

- The table remembers the registering row per namespace: a withdrawn namespace must answer `reloading` or `unavailable` rather than fall through to a parent namespace.
- `call` takes the caller as data, not as a connection handle: the reply ordering belongs to Connection, and the table never writes a socket.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ManagementMethods is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: one table serves every connection. |
| §3 orthogonality | No other owner's state is read; the loader's row status is the mechanism's read. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use `register` and `call`; no caller sees the registration table. |
| §8 rule | An unloading table settles its accepted calls before its services go. |
| §7 no proxy | The handler's error data is returned unchanged; the table adds no business meaning. |
| ADR 002 D1 | Process scope: namespaces belong to process rows. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | No dependency on providers; a disabled or failed provider affects only its namespaces. |
| ADR 002 D4 | The namespace set is structural: rows bring their namespaces. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Session registration (#2026)**: the Session method table takes the same `call` request and reply data, so Rpc has one call path for both tables.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerRef { pub connection_id: String, pub call_id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    pub method: String,
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<CallerRef>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallReply { pub reply: serde_json::Value }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interruption { Report, Drain }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodDecl {
    pub name: String,
    pub mutating: bool,
    pub interruption: Interruption,
    #[serde(default)]
    pub notifications: Vec<String>,
    #[serde(default)]
    pub progress: bool,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum TableError {
    #[error("{message}")]
    Reserved { message: String },
    #[error("{message}")]
    Duplicate { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("Unknown method")]
    UnknownMethod,
    #[error("Service is reloading; retry")]
    Reloading,
    #[error("Service is unavailable until it is fixed or restarted")]
    Unavailable,
    #[error("Service reloaded; the call may have committed")]
    Interrupted,
}

#[rutis::service(name = "managementMethods")]
impl MethodTable {
    pub async fn register(&self, request: RegisterRequest) -> Result<(), TableError> { /* ... */ }
    pub async fn call(&self, request: CallRequest) -> Result<CallReply, TableError> { /* ... */ }
}
```

`RegisterRequest.call` is a callback by reference: the handler runs on the registering row's runtime and is never called inside a commit lock.
