# UiHost

Scope: process. Issue: #2021.

UiHost owns the daemon's interface registrations: which interfaces are alive on which Session, their generations, their one-time attach tokens, and the UI leaf sessions the daemon mounts for interactive `dimcode` processes. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/ui_host.rs` and `ui_leaf.rs`, and its behaviour is described in the [Tui design](Tui.md#36-ui-registry-and-ui-leaves). Connection, Rpc and UiExtensions are separate owners.

## Use and scope: responsibility split

UiHost owns:

- **Registrations.** One record per interface: the Session it views, its kind, its generation, and the connection that registered it.
- **Leaves.** The rutis session mounted for an interactive leaf, its one-time token, its twenty-second start window, and its disposal when the session or the interface ends.
- **Replacement.** Claiming the interface a replacement replaces, and closing it once the replacement attaches.
- **Notices.** The advisory close notice on the interface's connection.
- **Attribution.** The fact `uiHost/attribution {connection_id, session_id, kind}` when a registration binds a connection, and `uiHost/attribution-ended {connection_id}` when it clears. Connection folds the fact into the presence value; UiHost's registry is never read by another owner.

Callers keep:

- Whether the calling connection views the Session: Connection answers `attached`.
- Writing to the connection: Connection writes the advisory notice.
- What an interface shows: the client's own tree.
- The UI content catalog: UiExtensions owns it; UiHost only names the kind.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client opens an interface on a Session it views | No: the token, the generation and the leaf session are the owner's | `open` |
| A connection registers itself on a Session | No: one registration per connection and Session and the generation are the owner's | `attach` |
| A connection replaces another interface | No: the claim and the expected generation are the owner's | `replace` |
| A client lists the live interfaces | Yes: `status` composes | `status` |
| A client or the daemon closes an interface | No: the notice, the leaf disposal and the registration removal are one settlement | `close` |
| The daemon advises an interface to leave without closing it | No: the notice is the owner's | `shutdown` |
| A connection stopped viewing a Session | No: the registration's connection binding is the owner's | `detach` |
| A connection ended | No: every registration naming it is the owner's | `release` |

An operation or parameter exists only where the row says No.

## Caller example

Rpc opens an interface for a leaf that is waiting on its address:

```rust
let opened = ui_host.open(OpenRequest {
    connection_id: caller.clone(),
    session_id,
    kind: "tui".into(),
    peer,
    address,
    replace, // the interface the client confirmed replacing; expected_generation is optional
    expected_generation: None,
}).await?;
// The reply carries the one-time token the leaf passes to `attach`.
```

A browser registers a plain interface:

```rust
let attached = ui_host.attach(AttachRequest {
    connection_id: caller, session_id, kind: "web".into(), ui_id: None, token: None,
}).await?;
```

## Own contract

### Dependencies

Connection's `attached` and `write`; rutis' `Ctx::plugin` on the owner's own process context, which mounts the leaf session inside this owner's subtree. No other owner provides a mount operation.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `open` | Reserve an interface and mount a leaf's session | `{connection_id, session_id, kind, peer, address, replace?, expected_generation?}` | `{ui_id, generation, token}` | While the leaf session is mounted; a failed start leaves nothing registered | `session_not_attached`, `invalid_params`, `ui_unknown`, `ui_generation`, `ui_leaf_failed` |
| `attach` | Register a connection as an interface of a Session | `{connection_id, session_id, kind, ui_id?, token?}` | `{ui_id, generation}` | None | `session_not_attached`, `invalid_params`, `ui_token`, `ui_in_use`, `ui_unknown` |
| `replace` | Replace an interface with a new registration | `{connection_id, session_id, kind, ui_id, expected_generation?}` | `{ui_id, generation}` | None | `session_not_attached`, `invalid_params`, `ui_unknown`, `ui_generation` |
| `status` | The live interfaces and the count of web connections | `{}` | `{instances, web_connections}` | None | none |
| `close` | Remove an interface; a leaf's session ends | `{ui_id, reason?}` | `{}` | None; the owner settles the notice and the leaf | `ui_unknown` |
| `shutdown` | Advise an interface to leave, without closing it | `{ui_id, reason?}` | `{}` | None | `ui_unknown`, `ui_detached` |
| `detach` | A connection stopped viewing a Session | `{connection_id, session_id}` | `{}` | None | none |
| `release` | A connection ended | `{connection_id}` | `{}` | None | none |

`open` and `attach` require that `connection_id` views `session_id`; the owner asks Connection rather than reading its attachments.

The owner emits two process events: `uiHost/attribution {connection_id, session_id, kind}` when a registration binds a connection and `uiHost/attribution-ended {connection_id}` when it clears. Connection folds them into its presence value; no other owner reads the registry.

The `ui.extensions.changed {revision}` notification is Connection's delivery of UiExtensions' event to clients; UiHost plays no part in it.

`expected_generation` is the generation the client read from `status` when it asked the user to confirm a replacement. It is a business version, not an owner-internal revision: the owner cannot decide it, because a client that saw interface A may have confirmed replacing A, and replacing whatever is current instead would close an interface the user never saw. The claim is atomic in `open`/`replace`; the attach token authorizes the leaf's attach and is a different value.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `session_not_attached` | Attach to the Session before registering an interface |
| `invalid_params` | Fix the request: an unsupported kind, a missing address, a malformed peer, a token without an id |
| `ui_token` | The one-time token is wrong; the leaf cannot attach |
| `ui_in_use` | Another connection already is this leaf; close it first or replace it |
| `ui_unknown` | The interface is gone; list the live interfaces |
| `ui_generation` | The interface changed since the caller looked; re-read `status` |
| `ui_leaf_failed` | The leaf did not start; nothing is registered |
| `ui_detached` | The interface is between a detach and its reattach; it can reattach and receive a later notice |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `open` | Check the attachment, reserve the registration and the token, mount the leaf session and record its view | The reservation is removed and the leaf is disposed | The leaf ends with its session | Every leaf is disposed and every registration removed | UiHost: the leaf session |
| `attach` | Check the attachment, verify the token, bind the connection, close the replaced interface once attached | — | The connection binding is released | Registrations are removed | UiHost |
| `close` | Remove the registration, write the notice, dispose the leaf, wake presence | — | — | The registrations are already gone | UiHost: the notice and the leaf; Connection: the frame |
| `detach`, `release` | Clear the connection binding; a leaf stays and can reattach | — | — | — | UiHost |

## Exceptions with reasons

- `open` takes the leaf's waiting address as a string: the leaf chose it and the daemon dials it; no file or socket handle crosses.
- The twenty-second start window is the owner's state: a leaf that does not attach is closed as `close` would.
- `expected_generation` stays a request field: it is the version the client observed and confirmed, so the owner cannot choose it; a stale client is refused with `ui_generation` instead of silently replacing a newer interface.

## Supersedes

- [Tui design](Tui.md#36-ui-registry-and-ui-leaves) §3.6: the registry's state and operations are those in this contract; the leaf lifecycle description stays.
- [Service faces](../service-faces.md) §3.6 row `uiHost`.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | UiHost is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: registrations and leaves outlive Connections and Sessions' branches. |
| §3 orthogonality | No other owner's state is read; Connection answers attachment and writes notices. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the registration map. |
| §7 no handle | Every operation is addressed by `connection_id` or `ui_id`; the leaf is mounted inside the owner. |
| ADR 002 D1 | Process scope: one registry per daemon. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Connection is asked through its operations. |
| ADR 002 D4 | No business setting. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Connection (#2021)**: `attached` and `write`, used by this owner and defined here as dependencies.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub connection_id: String,
    pub session_id: String,
    pub kind: String,
    pub peer: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenReply { pub ui_id: String, pub generation: u64, pub token: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    pub ui_id: String,
    pub session_id: String,
    pub kind: String,
    pub since_ms: u64,
    pub generation: u64,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReply { pub instances: Vec<Instance>, pub web_connections: usize }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum UiError {
    #[error("Attach to the Session before registering an interface on it")]
    SessionNotAttached,
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("The UI leaf token is not valid")]
    UiToken,
    #[error("Another connection already is this UI leaf")]
    UiInUse,
    #[error("No such interface is registered")]
    UiUnknown,
    #[error("The UI generation is not current")]
    UiGeneration,
    #[error("{message}")]
    UiLeafFailed { message: String },
    #[error("This UI leaf is between a detach and its reattach")]
    UiDetached,
}

#[rutis::service(name = "uiHost")]
impl UiHost {
    pub async fn open(&self, request: OpenRequest) -> Result<OpenReply, UiError> { /* ... */ }
    pub async fn attach(&self, request: AttachRequest) -> Result<AttachReply, UiError> { /* ... */ }
    pub async fn replace(&self, request: ReplaceRequest) -> Result<AttachReply, UiError> { /* ... */ }
    pub async fn status(&self, request: Empty) -> StatusReply { /* ... */ }
    pub async fn close(&self, request: CloseRequest) -> Result<(), UiError> { /* ... */ }
    pub async fn shutdown(&self, request: ShutdownRequest) -> Result<(), UiError> { /* ... */ }
    pub async fn detach(&self, request: DetachRequest) { /* ... */ }
    pub async fn release(&self, request: ReleaseRequest) { /* ... */ }
}
```

`ui_detached` is the `shutdown` error for an interface between a detach and its reattach; the leaf can reattach and receive a later notice.
