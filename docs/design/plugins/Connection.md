# Connection

Scope: process. Issue: #2021.

Connection owns the process's live client connections. This document is the owner's target contract; the current implementation is described in the [Tui design](Tui.md#36-ui-registry-and-ui-leaves) and the [App Server assembly](../../architecture/rust-agent/generated/app-server/services.md). Transport, Rpc, UiHost, CallChannel, UiExtensions, UiResources and Report are separate owners with their own contracts; their calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

Connection owns:

- **Admission.** Whether a client may join now: the hello/attach admission that races with the idle-close decision, and the connection limit inside a frame budget.
- **Connections.** Each accepted client's reader, writer, outbound queue and reply ordering. A connection lives until its reader ends, its client closes, or the daemon closes.
- **Session attachments.** Which Sessions a connection is viewing, each holding one Session lease and one broadcast subscription. Detaching is not closing a Session.
- **Presence.** The live clients of one Session, as the value the model-facing branch reads.
- **The inbound handover.** Each frame is handed to Rpc's current generation; Connection never interprets a method.

Connection keeps no Timeline fact, no Session state and no protocol meaning.

Callers keep:

- HTTP routing, authentication and the web entry's own routes: WebEntry decides which request becomes a connection upgrade.
- Protocol semantics: hello fields, built-in methods, Session routing and method errors are Rpc's.
- Listener binding and endpoint cleanup: Transport owns the sockets that produce accepted streams.
- The UI registry: UiHost owns interface registrations; Connection tells UiHost that a connection ended.
- The model-facing client snapshot: EnvironmentClientsBranch owns the `<dim-state>` text; Connection gives the presence value.
- The decision to close the daemon: Rpc's `daemon.shutdown` reads the facts each owner publishes; Connection contributes the connection counts and commits the refusal.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Transport and WebEntry need an accepted stream to become a live connection | No: creating the record, admitting it against the idle-close decision and owning the read loop are the owner's; the stream is the one handover | `accept` |
| A frame must reach the protocol generation that is current now | No: storing the handler in Connection makes a reload a stale object; the owner asks Rpc per frame | dependency on Rpc's `hello` and `request` |
| A provider or Rpc must write a frame after the reply of a call | No: the reply ordering is the connection's outbound queue | `write`, `write_after` |
| A client views a Session | No: the attachment holds one Session lease and one subscription, released on every path | `attach`, `detach` |
| UiHost needs to know whether a connection views a Session | Yes: a read composes | `attached` |
| The model needs the live clients of a Session | No: the value must come from the owner's own records, not from another owner's registry | `presences` |
| An idle close needs connection counts and an atomic refusal | No: counting and refusing in two calls races with hello and attach | `counts`, `close_for_idle` |
| The daemon closes every connection and waits for its tasks | No: the owner settles the writers and readers | `close_all` |
| A connection ends and its attachment and UI registration must go | No: one settlement releases the lease, the subscription and the UI registration | `settle` |

An operation or parameter exists only where the row says No.

## Caller example

Transport accepts a stream and hands it over:

```rust
// Refused only while the daemon is closing; nothing was created.
let connection = match connections.accept(stream, Entry::Local).await {
    Ok(connection) => connection,
    Err(AcceptError::Closing) => return,
};
```

EnvironmentClientsBranch reads the presence value and writes its own input:

```rust
let presences = connections.presences(session_id.clone()).await?;
let body = render(&presences); // the branch owns the text
append_snapshot(&timeline, body)?;
```

The idle-close handler reads the other owners' facts, then commits the refusal in one call:

```rust
let running = agents.sessions().await?.sessions.iter().filter(|s| s.running).count();
let held = process_activity.holds().await?.count; // a count, not a guard
match connections.close_for_idle(CloseForIdle {
    exclude_connection_id: caller.clone(),
    running_sessions: running,
    held_work: held,
}) {
    Ok(counts) => {
        // The refusal is committed; the assembly quiesces Agents and closes.
        shutdown.request(ShutdownMode::Graceful);
    }
    Err(CloseForIdleError::Busy(busy)) => return Err(busy.into()),
}
```

A provider that notifies the calling connection uses CallChannel, which calls `write_after`:

```rust
channel.notify(NotifyRequest { connection_id, call_id, method, params }).await?;
```

## Own contract

### Dependencies

Rpc's current generation (`hello`, `request`), read by name on each frame; UiHost's `detach` and `release`; Agents' `acquire {sessionId} → {leaseId, generation}`, `release {leaseId}` and `sessions` ([Agents contract](Agents.md)).

### Operations

The owner type is `ConnectionHub`. `accept` is crate-internal because a socket cannot be data; every other operation is on the `connection` face.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `accept` (crate-internal) | Turn an accepted stream into a live connection | `{stream, entry}` | `{connection_id, generation}` | None; the caller either accepts it or drops the stream | `closing` |
| `write` | Write one frame to a connection | `{connection_id, frame}` | `{}` | None; the frame is queued or refused | `unknown_connection`, `gone`, `too_large`, `slow` |
| `write_after` | Write one notification after the reply of one call | `{connection_id, call_id, frame}` | `{}` | None | `unknown_connection`, `gone`, `too_large`, `slow` |
| `write_deferred` | Write one frame when the connection settles, before its writer ends | `{connection_id, frame}` | `{}`; a connection that already settled drops the frame | None | none |
| `closed` | Wait until a connection ends | `{connection_id}` | `{}` when the connection ends; an unknown id resolves at once | The caller's task ends | none |
| `attach` | A connection views a Session | `{connection_id, session_id}` | `{generation}` | While the Session lease is acquired and the listener registered; nothing is attached | `unknown_connection`, `closed`, `session_unavailable`, `cancelled` |
| `detach` | A connection stops viewing a Session | `{connection_id, session_id}` | `{}` | None; the owner completes the release | none |
| `attached` | Whether a connection views a Session now | `{connection_id, session_id}` | `{attached}` | None | none |
| `presences` | The live clients of a Session | `{session_id}` | `{presences: [{channel, label?}]}` | None | none |
| `counts` | Live connections and attached Sessions, excluding one connection | `{exclude_connection_id}` | `{connections, attached_sessions}` | None | none |
| `close_for_idle` | Commit the daemon's refusal of new clients when nothing uses it | `{exclude_connection_id, running_sessions, held_work}` | `{counts}` | None | `busy{connections, attached_sessions, running_sessions}` |
| `close_all` | Close every connection and wait for its tasks | `{}` | `{}` | None; the owner completes the close | none |
| `settle` (internal) | One connection ended: release its attachment, tell UiHost, end its writer | `{connection_id, reason}` | `{}` | None | none |

`write` and `write_after` are one operation family: `write_after` exists because a notification sent while a call runs must follow that call's reply, and only the connection's outbound queue knows the reply has been written. `write` refuses a frame over the connection's frame budget with `too_large`, and closes a connection whose reliable allowance is exhausted, reporting `slow` to the writer of that frame.

`close_for_idle` reads the other owners' facts from its request: `running_sessions` from Agents' `sessions` value and `held_work` from `processActivity.holds`, both read immediately before. The owner rechecks only its own counts under the admission lock and commits the refusal in the same step, so no hello or attach joins after the decision.

After the commit the assembly asks Agents to `quiesce` every Session. A `running` answer means a Session accepted work between the read and the pause; the refusal stands and the graceful close settles that work — admission is never reopened after the commit.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `closing` | The daemon has refused new clients; stop accepting on this listener |
| `unknown_connection` | The connection is gone; drop the write or the attachment and read the current state |
| `gone` | The connection closed or its generation unloaded; nothing was written |
| `too_large` | The frame exceeds the connection's frame budget; the connection stays open — the provider must shorten its notification |
| `slow` | The connection's reliable allowance is exhausted; the connection is closed and the caller drops the frame |
| `session_unavailable` | The Session cannot be acquired now; report it to the client and retry the attach later |
| `cancelled` | Nothing was attached; the caller stopped waiting |
| `closed` | This generation is unloading; reconnect to the replacement generation |
| `busy` | Somebody is using the daemon; answer the client with the three counts and do not close |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `accept` | Admit under the lock, create the record and the writer, spawn the reader, count it | The stream is dropped before the record exists; nothing was admitted | The reader ends with the stream | New accepts are refused as `closing`; every connection is closed | Connection: the record, reader and writer |
| `write`, `write_after` | Encode once, admit into the outbound queue, write in order | None; a frame is queued or refused | A closed connection is `gone` | Pending writes end `gone` | Connection: the queue and writer |
| `write_deferred` | Hold the frame until the connection settles | — | — | `close_all` writes it after the queue drains and before the writer ends | Connection |
| `closed` | Resolve when the reader or the owner ends the connection | — | The waiter ends | Resolves for every waiter | Connection |
| `attach` | Acquire the Session lease, register the subscription, attach under the connection lock | Before the reply: lease released, listener disposed, nothing attached | The attachment is released with the connection | Pending calls end `closed`; every attachment is released | Connection: the attachment; Agents: the lease |
| `detach` | Remove the attachment and release its lease and listener | — | — | The attachment is already gone | Connection |
| `close_for_idle` | Recheck counts under the lock, set the refusal, return the counts; the caller then asks Agents to quiesce | None | — | The refusal is the close; a `running` quiesce answer does not reopen it | Connection |
| `close_all` | Tell UiHost the connections ended, cancel each reader and writer, wait for the tasks | — | — | It is the close | Connection |

## Exceptions with reasons

- `accept` takes an accepted stream: a socket cannot be represented as data. The shared lifecycle is fixed here: Transport creates the stream by accepting on a listener it owns, calls `accept` once, and never reads or writes the stream afterwards; Connection owns it from the call boundary and settles it in `settle` or `close_all`; when Transport reloads, the listeners close and existing connections stay with Connection; when Connection reloads, every connection settles and Transport's accept calls are refused as `closing` until the new generation is installed.
- Attachment leases are agents' ids, not guards: the connection stores the lease id and releases it in one settlement path, so a connection that dies on any path leaves no lease.
- `presences` labels are self-declared and never identities; the value says so by naming the field `label`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Connection is one process part. It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | Process. A connection is shared by every Session it views; attachments are the Session-facing part of the same resource. |
| §3 orthogonality | No other owner's state is read. UiHost and Agents are asked through their operations; presence is Connection's own projection over its records. |
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
| §7 no proxy | Presence is computed from Connection's own records; the UI-leaf attribution arrives as UiHost's fact, not as a read of its registry. |
| ADR 002 D1 | Process scope: connections outlive Sessions and are shared by them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point to extensions: Rpc, Transport, UiHost and Agents are used as operations; none is stored as an object. |
| ADR 002 D4 | The listener configuration is structural; no business setting is involved. |
| ADR 002 D5 | The contract is language-neutral except `accept`; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Agents (#2020)**: the operations are the ones in the Agents contract ([#2098](https://github.com/arcships/dim/pull/2098)): `acquire {sessionId} → {leaseId, generation}` and `release {leaseId}` for the attachment lease, `sessions` for the running count, and `quiesce` for the shutdown pause. No new operation is required; the caller passes the read values into `close_for_idle` and quiesces after the commit.
- **rutis (runtime)**: none.

## Binding appendix

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencesRequest { pub session_id: String }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencesReply { pub presences: Vec<ClientPresence> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts { pub connections: usize, pub attached_sessions: usize }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Busy {
    pub connections: usize,
    pub attached_sessions: usize,
    pub running_sessions: usize,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CloseForIdleError {
    #[error("The App Server is in use")]
    Busy(Busy),
}

#[rutis::service(name = "connection")]
impl ConnectionHub {
    pub async fn write(&self, request: WriteRequest) -> Result<(), WriteError> { /* ... */ }
    pub async fn write_after(&self, request: WriteAfterRequest) -> Result<(), WriteError> { /* ... */ }
    pub async fn write_deferred(&self, request: DeferredRequest) { /* ... */ }
    pub async fn closed(&self, request: ClosedRequest) { /* waits */ }
    pub async fn attach(&self, request: AttachRequest) -> Result<AttachReply, AttachError> { /* ... */ }
    pub async fn detach(&self, request: DetachRequest) { /* ... */ }
    pub async fn attached(&self, request: AttachedRequest) -> AttachedReply { /* ... */ }
    pub async fn presences(&self, request: PresencesRequest) -> PresencesReply { /* ... */ }
    pub async fn counts(&self, request: CountsRequest) -> Counts { /* ... */ }
    pub async fn close_for_idle(&self, request: CloseForIdleRequest) -> Result<Counts, CloseForIdleError> { /* ... */ }
    pub async fn close_all(&self, request: Empty) { /* ... */ }
}

impl ConnectionHub {
    /// Crate-internal: a socket cannot be data. See the exception in this contract.
    pub(crate) async fn accept(&self, stream: ClientStream, entry: Entry) -> Result<AcceptReply, AcceptError> { /* ... */ }
    pub(crate) async fn settle(&self, connection_id: &str, reason: SettleReason) { /* ... */ }
}
```

`ClientStream` is the accepted socket (Unix stream, named pipe or WebSocket); it never crosses a language boundary. `Entry` is `local`, `bare` or `web`.
