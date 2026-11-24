# ManagementMethods

Scope: process. Issue: #2021.

ManagementMethods owns the process namespace table: which namespaces a process part registered, what each declared, and who answers a name now. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/methods.rs` and `crates/dim-agent-capabilities/src/methods.rs`. Rpc, the Session method table ([#2026](https://github.com/arcships/dim/issues/2026)) and the registering parts are separate owners. The method declarations, interruptions and reply/error shapes are the [methods contract](../contracts/session-boundary.md) A.1; this document states the owner's operations.

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
| A process part publishes its namespace for the row's lifetime | No: the registration's lifetime, the duplicate check and the row-bound cleanup are the owner's | `register` |
| Rpc must dispatch one call to the namespace that holds the name now | No: the lookup, the caller identity, the interruption and the withdrawn-row answer are the owner's | `call` |

An operation or parameter exists only where the row says No.

## Caller example

A process part registers from `apply`:

```rust
table.register(
    ctx,
    "settings".into(),
    declarations,
    Arc::new(move |call: MethodCall, cancelled: CancellationToken, progress: Progress| {
        let owner = owner.clone();
        // The token fires on the provider's or the admitting Rpc load's unload (A.1).
        Box::pin(async move { owner.handle(call, cancelled, progress).await }) // Result<MethodReply, MethodCallbackError>
    }),
).await?;
```

Rpc dispatches:

```rust
// `service_reloading` and `service_unavailable` are answered from the registering row's status.
let reply = table.call(
    method.into(),
    params,
    Some(CallerRef { connection_id, call_id }),
    cancelled,
).await?;
answer_reply(reply) // MethodReply: the handler's value or its own {code, message}
```

## Own contract

### Dependencies

The loader's row status (`rutis/loader`), read when a withdrawn namespace answers; the registering rows' `ctx` for the registration lifetime.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a namespace for the registering row's lifetime | `{namespace, declarations}` and a call callback | `{}` | None; the owner completes it | `reserved`, `duplicate`, `invalid` |
| `call` | Dispatch one call to the namespace that holds it | `{method, params, caller?}` | `{reply}`: the handler's `MethodReply` | The calling load's token (A.1); a caller disconnect stops delivery only and does not cancel the accepted call | `unknown_method`, `reloading`, `unavailable`, `interrupted`, `invalid`, `closed` |

`caller` is `{connectionId, callId}`: the connection the call arrived on and the call id the reply ordering uses. It is absent for an in-process call. The table passes it to the handler unchanged; a handler that notifies uses it with `callChannel`.

The handler's own failure is `MethodReply`'s error data (A.1's `{code, appError, message}`), never a table error; the table returns only its routing failures. The operations, `MethodCall`, `MethodReply` and `MethodError` are the [SessionMethods](https://github.com/arcships/dim/pull/2103#own-contract) contract's (#2103), the same shape both tables serve.

`register`'s `call` is a callback parameter, not a request field, and the registration is bound to `caller`'s row: the table removes it when that row unloads. The callback runs on the registering row and is never called inside a commit lock. Its handler signature is A.1's `(MethodCall, CancellationToken, Progress)`, so the token fires on either unload, `drain` calls included; `CancellationToken` is the runtime's cancellation token and `Progress` is A.1's per-call progress sink (Appendix F). The handler returns `MethodReply` with `MethodCallbackError: From<rutis::host::CallError>`.

### Errors

The wire codes are the existing ones from the methods contract.

| Kind | Wire code | What the caller does next |
| --- | --- | --- |
| `reserved` | — (registration, in-process) | The namespace or a name is the host's; pick another name |
| `duplicate` | — (registration, in-process) | The namespace is registered already; the registering row must not register twice |
| `invalid` | `invalid_params` | The namespace or declaration is outside the name grammar, or the request is malformed |
| `unknown_method` | `unknown_method` | No registered or withdrawn namespace holds the name; the client sees an unknown method |
| `reloading` | `service_reloading` | The registering row is starting or reloading; retry after it is Active |
| `unavailable` | `service_unavailable` | The registering row failed or cannot run; the client sees the service as unavailable |
| `interrupted` | `operation_interrupted` | The provider unloaded while the call ran; the call may have committed, so read the current state before retrying |
| `closed` | — | This generation is unloading; the next generation serves the call |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the name and declarations, record them under the row, refuse a duplicate | — | The registration is withdrawn and the row recorded | New registrations are refused; the table reloads with the process | The registering row |
| `call` | Find the owner, admit the call, run the handler, return its outcome; on unload, interrupt `report` and wait for `drain` | The caller stops waiting; the accepted call continues and settles by its declaration | The call ends and its commands settle | A withdrawn namespace answers from its row's status | The table for the call's settlement; the handler for its effects |

## Exceptions with reasons

- The table remembers the registering row per namespace: a withdrawn namespace must answer `reloading` or `unavailable` rather than fall through to a parent namespace.
- `call` takes the caller as data, not as a connection handle: the reply ordering belongs to Connection, and the table never writes a socket.

## Supersedes

None. The declarations, interruptions and reply/error shapes stay with the [methods contract](../contracts/session-boundary.md) A.1; this contract states the table owner's operations.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.

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

- **Session registration (#2026)**: the Session method table's shape is [SessionMethods](https://github.com/arcships/dim/pull/2103) (#2103), the owner's latest head; Rpc calls each table by its own contract. This contract covers only the management table.

## Binding appendix

The operation shapes and their data types are the [SessionMethods](https://github.com/arcships/dim/pull/2103#own-contract) contract's (#2103): `MethodDecl`, `MethodCall`, `MethodReply`, `MethodCallbackError`, `MethodError`, `CallerRef` and `Interruption`. `Progress` is the per-call sink in the methods contract's Appendix F. This contract adds only the process table's binding; it declares no second copy of those types.

```rust
use crate::host::CancelledFn;
use crate::methods::{
    CallerRef, MethodCall, MethodCallbackError, MethodDecl, MethodError, MethodReply, Progress,
};
use rutis::Ctx;

pub type HandlerFn = Arc<
    dyn Fn(MethodCall, CancellationToken, Progress) -> BoxFuture<'static, Result<MethodReply, MethodCallbackError>>
        + Send + Sync,
>;

/// The process table's native binding. `managementMethods` is a shared catalog name
/// with a hand-written `HostDispatch` at `host_key("managementMethods")`: the handler
/// carries a cancellation token and a progress sink, which are not serde data, so the
/// marked-service macro cannot bind `register`. Rust callers use this trait directly;
/// other languages register through the same dispatch.
pub trait ManagementTable {
    fn register(
        &self,
        caller: &Ctx,
        namespace: String,
        declarations: Vec<MethodDecl>,
        call: HandlerFn,
    ) -> BoxFuture<'static, Result<(), MethodError>>;

    fn call(
        &self,
        method: String,
        params: serde_json::Value,
        caller: Option<CallerRef>,
        cancelled: CancelledFn,
    ) -> BoxFuture<'static, Result<MethodReply, MethodError>>;
}
```

`ManagementTable` is the native spelling; the catalog name is `managementMethods`. The same table serves `call` in process and on the wire.
