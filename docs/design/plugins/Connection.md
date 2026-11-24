# Connection

Scope: process. Issue: #2021.

Connection owns the process's live client connections. This document is the owner's target contract; the current implementation is described in the [Tui design](Tui.md#36-ui-registry-and-ui-leaves) and the [App Server assembly](../../architecture/rust-agent/generated/app-server/services.md). Transport, Rpc, UiHost, CallChannel, UiExtensions, UiResources and Report are separate owners with their own contracts; their calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

Connection owns:

- **Admission.** Whether a client may join now: the hello/attach admission that races with the idle-close decision, and the connection's frame and reply budgets.
- **Connections.** Each accepted client's reader, writer, outbound queue and reply ordering. A connection lives until its reader ends, its client closes, or the daemon closes.
- **Session attachments.** Which Sessions a connection is viewing, each holding one Agents lease and one broadcast subscription. Detaching is not closing a Session.
- **Presence.** The live clients of one Session, as the value the model-facing branch reads. The leaf attribution is folded from UiHost's event; Connection never reads the UI registry.
- **The inbound handover.** Each frame is handed to Rpc's current generation; Connection never interprets a method.
- **The two facts and one notice.** `ClientsChanged` wakes the presence branch when a Session's audience changes; `uiExtensions/changed` carries the UI content revision; the `ui.extensions.changed` notification carries it to clients.

Connection keeps no Timeline fact, no Session state and no protocol meaning.

Callers keep:

- HTTP routing, authentication and the web entry's routes: WebEntry decides which request becomes a connection upgrade.
- Protocol semantics: hello fields, built-in methods, Session routing and method errors are Rpc's.
- Listener binding and endpoint cleanup: Transport owns the sockets that produce accepted streams.
- The UI registry: UiHost owns interface registrations and emits the attribution event; Connection folds it.
- The model-facing client snapshot: EnvironmentClientsBranch owns the `<dim-state>` text.
- The decision to close the daemon: Rpc's `daemon.shutdown` reads the facts each owner publishes; Connection contributes the connection counts and commits the refusal.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Transport and WebEntry need an accepted stream to become a live connection | No: creating the record, admitting it against the idle-close decision and owning the read loop are the owner's; the stream is the one handover | `accept` |
| A frame must reach the protocol generation that is current now | No: storing the handler in Connection makes a reload a stale object; the owner asks Rpc per frame | dependency on Rpc's `hello` and `request` |
| Rpc must write a call's reply and release the notifications held behind it | No: the reply ordering and the reply's own budget are the connection's | `reply` |
| A provider or UiHost must write a notification, before or after a call's reply | No: the ordering and the notification budget are the connection's | `write`, `write_after` |
| A shutdown must answer the initiating client after the daemon settled | No: only the connection can write after its queues drain | `write_deferred` |
| A client views a Session | No: the attachment holds one Agents lease and one subscription, released on every path | `attach`, `detach` |
| UiHost needs to know whether a connection views a Session | Yes: a read composes | `attached` |
| The model needs the live clients of a Session, including which are UI leaves | No: the value must come from the owner's own records; the leaf attribution is folded from UiHost's event | `presences` |
| An idle close needs connection counts and an atomic refusal | No: counting and refusing in two calls races with hello and attach | `counts`, `close_for_idle` |
| The daemon closes every connection and waits for its tasks | No: the owner settles the writers and readers | `close_all` |
| A connection ends and its attachment and UI attribution must go | No: one settlement releases the lease and the subscription | `settle` |
| A part must wait for a specific connection to end | Yes: `closed` composes | `closed` |

An operation or parameter exists only where the row says No.

## Caller example

Transport accepts a stream and hands it over:

```rust
// Refused only while the daemon is closing; nothing was created.
let Some(connection) = connections.accept(stream, Entry::Local) else { return };
```

EnvironmentClientsBranch reads the presence value and writes its own input:

```rust
let presences = connections.presences(session_id.clone()).await?;
let body = render(&presences); // the branch owns the text
append_snapshot(&timeline, body)?;
```

The idle-close handler reads the other owner's count and commits the refusal in one call:

```rust
let running = agents.sessions().await?.sessions.iter().filter(|s| s.running).count();
match connections.close_for_idle(CloseForIdle { exclude_connection_id: caller.clone(), running }) {
    Ok(counts) => shutdown.request(ShutdownMode::Graceful),
    // The refusal is the protocol's `daemon_busy` with these three counts.
    Err(CloseForIdleError::Busy(busy)) => return Err(daemon_busy(busy)),
}
// A process-work refusal is Rpc's own: it answers `daemon_busy` with an explanation and no counts.
```

## Own contract

### Dependencies

Rpc's current generation (`hello`, `request`), read by name on each frame; UiHost's `detach`/`release` and its attribution event; Agents' `acquire {sessionId} → {leaseId, generation}` and `release {leaseId}` ([Agents contract, PR #2098](https://github.com/arcships/dim/pull/2098)); UiExtensions' revision event.

### Operations

The owner type is `ConnectionHub`. `accept` is crate-internal because a socket cannot be data; every other operation is on the `connection` face.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `accept` (crate-internal) | Turn an accepted stream into a live connection | `{stream, entry}` | `{connection_id, generation}` | None; the caller either accepts it or drops the stream | `closing` |
| `reply` | Write a call's reply and release the notifications held behind it | `{connection_id, call_id, frame}` | `{}` | None | `unknown_connection`, `gone`, `too_large`, `slow` |
| `write` | Write one notification with no call ordering | `{connection_id, frame}` | `{}` | None; the frame is queued or refused | `unknown_connection`, `gone`, `too_large`, `slow` |
| `write_after` | Write one notification after a call's reply | `{connection_id, call_id, frame}` | `{}` | None | `unknown_connection`, `gone`, `too_large`, `slow` |
| `write_deferred` | Write one frame when the connection settles, before its writer ends | `{connection_id, frame}` | `{}`; a connection that already settled drops the frame | None | none |
| `closed` | Wait until a connection ends | `{connection_id}` | `{}` when it ends; an unknown id resolves at once | The caller's task ends | none |
| `attach` | A connection views a Session | `{connection_id, session_id}` + `caller: &Ctx` | `{generation}` | The caller's cancellation: the lease is released and the listener disposed | `unknown_connection`, `closed`, `session_unavailable`, `cancelled`, `invalid_params` |
| `detach` | A connection stops viewing a Session | `{connection_id, session_id}` + `caller: &Ctx` | `{}` | None | `invalid_params` |
| `attached` | Whether a connection views a Session now | `{connection_id, session_id}` | `{attached}` | None | none |
| `presences` | The live clients of a Session | `{session_id}` | `{presences: [{channel, label?}]}` | None | none |
| `counts` | Live connections and attached Sessions, excluding one connection | `{exclude_connection_id}` | `{connections, attached_sessions}` | None | none |
| `close_for_idle` | Commit the daemon's refusal of new clients when nothing uses it | `{exclude_connection_id, running}` | `Counts` | None | `busy {connections, attached_sessions, running}` |
| `close_all` | Close every connection and wait for its tasks | `{}` | `{}` | None; the owner completes the close | none |
| `settle` (crate-internal) | One connection ended: release its attachment, tell UiHost, end its writer | `{connection_id, reason}` | `{}` | None | none |

`reply` writes the frame a call produced and releases the notifications `write_after` held behind it. `write` is for a notification with no call to follow (a UI close notice); `write_after` is for a notification sent while or after a call runs, ordered behind that call's reply. A notification over the frame budget is `too_large` and leaves the connection open; a notification or reply the reliable allowance cannot take is `slow` and closes the connection as a slow consumer.

`close_for_idle` takes `running` from Agents' `sessions` value, read immediately before. The owner rechecks only its own counts under the admission lock and commits the refusal in the same step, so no hello or attach joins after the decision.

The caller's sequence is Rpc's: read Agents' `sessions`, then `close_for_idle`; on `Busy` answer `daemon_busy` with the counts; the assembly then asks Agents to `quiesce`. Quiesce runs after the commit because Agents' pause is one-way; [#2098](https://github.com/arcships/dim/pull/2098)'s `Busy { running }` answer belongs to its own call site, and at this one a `running` answer is settled by the graceful close — admission is never reopened after the commit.

### Errors

| Kind | Wire code | What the caller does next |
| --- | --- | --- |
| `closing` | `daemon_busy` (no counts, with the explanation) | The daemon refused the stream before it was admitted; stop accepting on this listener |
| `busy` | `daemon_busy` with `connections`, `attachedSessions`, `running` | Answer the client with the three counts and do not close |
| `unknown_connection` | — (in-process) | The connection is gone; drop the write or the attachment and read the current state |
| `gone` | — (in-process) | The connection closed or its generation unloaded; nothing was written |
| `too_large` | — (in-process) | The frame exceeds the connection's frame budget; the connection stays open — shorten the notification |
| `slow` | — (in-process) | The connection's reliable allowance is exhausted; the connection is closed and the frame is dropped |
| `session_unavailable` | `session_closed` when the Session is closed, `service_unavailable` when it cannot be acquired | Report it to the client and retry the attach later |
| `cancelled` | — (in-process) | Nothing was attached; the caller stopped waiting |
| `closed` | `service_unavailable` | This generation is unloading; reconnect to the replacement generation |
| `invalid_params` | `invalid_params` | Fix the request: an unknown entry, a malformed id |

Only the kinds with a wire code reach a client. The others are in-process outcomes of a caller that is itself a part.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `accept` | Admit under the lock, create the record and the writer, spawn the reader, count it | The stream is dropped before the record exists; nothing was admitted | The reader ends with the stream | New accepts are refused as `closing`; every connection is closed | Connection: the record, reader and writer |
| `reply`, `write`, `write_after` | Encode once, admit into the outbound queue, write in order | None; a frame is queued or refused | A closed connection is `gone` | Pending writes end `gone` | Connection: the queue and writer |
| `write_deferred` | Hold the frame until the connection settles | — | — | `close_all` writes it after the queue drains and before the writer ends | Connection |
| `attach` | Acquire the Session lease, register the subscription, attach under the connection lock | Before the reply: lease released, listener disposed, nothing attached | The attachment is released with the connection | Pending calls end `closed`; every attachment is released | Connection: the attachment; Agents: the lease |
| `detach` | Remove the attachment and release its lease and listener | — | — | The attachment is already gone | Connection |
| `close_for_idle` | Recheck counts under the lock, set the refusal, return the counts; the caller then asks Agents to quiesce | None | — | The refusal is the close; a `running` quiesce answer does not reopen it | Connection |
| `close_all` | Tell UiHost the connections ended, cancel each reader and writer, wait for the tasks | — | — | It is the close | Connection |

## Exceptions with reasons

- `accept` takes an accepted stream: a socket cannot be represented as data. The shared lifecycle is fixed here: Transport creates the stream by accepting on a listener it owns, calls `accept` once, and never reads or writes the stream afterwards; Connection owns it from the call boundary and settles it in `settle` or `close_all`; when Transport reloads, the listeners close and existing connections stay with Connection; when Connection reloads, every connection settles and Transport's accept calls are refused as `closing` until the new generation is installed.
- Attachment leases are Agents' ids, not guards: the connection stores the lease id and releases it in one settlement path, so a connection that dies on any path leaves no lease.
- `presences` labels are self-declared and never identities; the value says so by naming the field `label`.
- The UI-leaf attribution is a fact UiHost publishes (`uiHost/attribution` with `{connection_id, session_id, kind}`, and `uiHost/attribution-ended` with `{connection_id}`), and Connection folds it into its own presence state. A read of UiHost's registry would be 代查 and a handle would cross.

## Client-visible changes

- `daemon.shutdown {ifIdle:true}` keeps the wire error `daemon_busy` with `connections`, `attachedSessions` and `running`; the field `running` counts Sessions with accepted work or an activity hold, as before.
- One narrow behaviour changes: when a Session accepts work between the caller's read and Agents' `quiesce`, the daemon closes gracefully instead of answering `daemon_busy`, because Agents' pause is one-way and cannot be reverted; the alternative order (quiesce before the commit) would leave a paused daemon running when the commit is refused. Accepted work still settles; the affected client is the CLI's `daemon stop --if-idle` and any start that stops an idle daemon. Maintaining the old refusal would need a revertable pause from Agents; this contract asks the maintainer to confirm the change.

## Supersedes

- [Service faces](../service-faces.md) §3.6 rows `connectionHub` and `serverControl`, and §4.9's `sessionClientsHost` shape: the object names are replaced by this face.
- [Process owner operations](../contracts/process-owner-operations.md) rows `Presence consumers → Connection`, `Activity consumers / daemon clients → ServerControl` and `UI clients → UiHost / UiExtensions`: the operations are those in this document, [ServerControl](ServerControl.md) and [UiHost](UiHost.md).

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Connection is one process part. It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | Process. A connection is shared by every Session it views; attachments are the Session-facing part of the same resource. |
| §3 orthogonality | No other owner's state is read. UiHost and Agents are asked through their operations and events. |
| §4 rule 1 | No command event: Connection writes no Timeline fact. |
| §4 rule 2 | No Timeline handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning: method names and replies are Rpc's. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No model context: the presence value reaches the model only through EnvironmentClientsBranch's input. |
| §4 rule 7 | Callers use these operations; no caller reads the connection index or the outbound queue. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact. |
| §7 object handover | `accept` is the only object handover; its lifecycle is the exception above. |
| §7 no proxy | Presence is computed from Connection's own records; the leaf attribution arrives as UiHost's event, not as a read of its registry. |
| ADR 002 D1 | Process scope: connections outlive Sessions and are shared by them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point to extensions: Rpc, Transport, UiHost and Agents are used as operations. |
| ADR 002 D4 | The listener configuration is structural; no business setting is involved. |
| ADR 002 D5 | The contract is language-neutral except `accept`; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Agents (#2020)**: the operations are the ones in the Agents contract ([PR #2098](https://github.com/arcships/dim/pull/2098)): `acquire {sessionId} → {leaseId, generation}` and `release {leaseId}` for the attachment lease, `sessions` for the running count, and `quiesce` for the shutdown pause.
- **UiHost (#2021)**: the attribution events `uiHost/attribution` and `uiHost/attribution-ended`, defined in [UiHost](UiHost.md).

## Binding appendix

`CancelledFn` is the crate's cancellation argument (`crate::host::CancelledFn`); a row-bound operation takes `caller: &Ctx` and registers its cleanup on that row.

```rust
use crate::host::CancelledFn;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencesRequest { pub session_id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencesReply { pub presences: Vec<ClientPresence> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountsRequest { pub exclude_connection_id: String }

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts { pub connections: usize, pub attached_sessions: usize }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseForIdle { pub exclude_connection_id: String, pub running: usize }

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Busy {
    pub connections: usize,
    pub attached_sessions: usize,
    pub running: usize,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CloseForIdleError {
    #[error("The App Server is in use")]
    Busy(Busy),
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ConnectionError {
    #[error("The daemon is closing")]
    Closing,
    #[error("No such connection")]
    UnknownConnection,
    #[error("The connection is closed")]
    Gone,
    #[error("The frame exceeds the connection's budget")]
    TooLarge,
    #[error("The connection cannot keep up")]
    Slow,
    #[error("The Session cannot be acquired")]
    SessionUnavailable,
    #[error("The call was cancelled")]
    Cancelled,
    #[error("The service is unloading")]
    Closed,
    #[error("{message}")]
    InvalidParams { message: String },
}

#[rutis::service(name = "connection")]
impl ConnectionHub {
    pub async fn reply(&self, request: ReplyRequest) -> Result<(), ConnectionError> { /* ... */ }
    pub async fn write(&self, request: WriteRequest) -> Result<(), ConnectionError> { /* ... */ }
    pub async fn write_after(&self, request: WriteAfterRequest) -> Result<(), ConnectionError> { /* ... */ }
    pub async fn write_deferred(&self, request: DeferredRequest) { /* ... */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
    pub async fn attach(&self, caller: &Ctx, request: AttachRequest, cancelled: CancelledFn)
        -> Result<AttachReply, ConnectionError> { /* ... */ }
    pub async fn detach(&self, caller: &Ctx, request: DetachRequest) -> Result<(), ConnectionError> { /* ... */ }
    pub async fn attached(&self, request: AttachedRequest) -> AttachedReply { /* ... */ }
    pub async fn presences(&self, request: PresencesRequest) -> PresencesReply { /* ... */ }
    pub async fn counts(&self, request: CountsRequest) -> Counts { /* ... */ }
    pub async fn close_for_idle(&self, request: CloseForIdle) -> Result<Counts, CloseForIdleError> { /* ... */ }
    pub async fn close_all(&self) { /* ... */ }
}

impl ConnectionHub {
    /// Crate-internal: a socket cannot be data. See the exception in this contract.
    pub(crate) async fn accept(&self, stream: ClientStream, entry: Entry) -> Result<AcceptReply, ConnectionError> { /* ... */ }
    pub(crate) async fn settle(&self, connection_id: &str, reason: SettleReason) { /* ... */ }
}
```

`ClientStream` is the accepted socket (Unix stream, named pipe or WebSocket); it never crosses a language boundary. `Entry` is `local`, `bare` or `web`. `ClientPresence` is `{channel, label?}` with the channel names `tui`, `cli`, `web`, `acp`, `remote`, `desktop`, `unknown`.
