# Session broadcast notices

Status: Implemented in C6 ([#49](https://github.com/arcships/dim/issues/49),
[#85](https://github.com/arcships/dim/pull/85)). Requirement:
[SessionTitle §9 R1](../plugins/SessionTitle.md#9-requirements-raised-to-mechanism-owners). Overview:
[§6.2 constraint 2](../overview.md#62-constraints), [Appendix B.2](../overview.md#b2-app-server-process-plugins) (Rpc),
[Appendix F](../overview.md#appendix-f-requirements-raised-to-mechanism-owners). Broadcast contract:
[event-driven flows §6](../../architecture/rust-agent/event-driven-flows.md#6-session-广播). First emitter: SessionStore
title metadata (S5b, [#60](https://github.com/arcships/dim/issues/60), [#96](https://github.com/arcships/dim/pull/96):
[title metadata](session-title-metadata.md)).

## 1. Summary

Process-level state about a Session, such as SessionStore's title, never enters the Timeline
([principles §4 rule 9](../../architecture/rust-agent/principles.md)). When clients must see a change of such state
live, its owner announces the change with a notice on the Session's broadcast channel after the change commits. Rpc
forwards the notice to the protocol v3 connections attached to that Session.

- **Record.** `SessionBroadcastEvent::Notice` carries `{source, kind, data}`. Neither the broadcast nor Rpc reads
  these fields.
- **Emitter.** `session_broadcast::notify(ctx, sessionId, notice)` emits on the existing channel and returns at
  once. A notice with no listener is dropped.
- **Wire.** `session.notice {sessionId, source, kind, data}` on protocol v3.
- **Not added.** No Timeline event, sequence, storage, replay, plugin, lifecycle hook, client method or protocol
  version. A C4 leaf part that observes events skips notices (§6).

## 2. Flow

```text
 owner (process scope)          Session channel session/<id>       Rpc subscription            client (v3)
 e.g. SessionStore              one rutis named channel            one per attached connection
 -------------------------      ----------------------------       -------------------------   ------------------
 commit the change
 notify(ctx, id, notice) -----> Notice {source, kind, data}
                                  no listener: dropped
                                  (not loaded, not observed)
 SessionBroadcast (branch) ---> Event {branchId, event}
                                  |
                                  | one channel, emission order
                                  +------------------------------> Forward
                                  |                                  v1 or v2: no notice
                                  |                                  after Closed: dropped
                                  |                                  otherwise ---------------> session.notice
                                  |                                                              {sessionId, source,
                                  |                                                               kind, data}
                                  +------------------------------> C4 leaf rows: skip Notice
```

## 3. The notice record

```rust
pub enum SessionBroadcastEvent {
    Event { branch_id: String, event: Arc<AgentEvent> },
    /// A committed change of process-level state about this Session.
    Notice(Arc<SessionNotice>),
    Fence { recipient: String, token: u64 },
    Closed,
}
#[derive(Debug, Serialize)]
pub struct SessionNotice {
    pub source: String,           // the owner, e.g. "SessionStore"
    pub kind: String,             // the owner's kind of change, e.g. "title"
    pub data: serde_json::Value,  // the owner's data for that kind
}
```

| Rule | Content |
| --- | --- |
| Meaning | Defined by the owner. `source` names the owner, and the owner documents what each `kind` and its `data` mean (the title notice: [S5b §7](session-title-metadata.md#7-the-title-notice)). The broadcast and Rpc never interpret them, and a client that does not handle a `source` or `kind` ignores it. |
| Session | The record does not name the Session; the channel does, as for `Event`. Owners need not repeat the Session id in `data`. |
| Position | No sequence number and no batch; never persisted or replayed. A notice reports that state changed; it is not a fact that later decisions use. An owner whose clients must order or deduplicate its changes puts its own revision in `data`, as the title notice does. |
| Body | Every listener shares one body through `Arc`, as for `Event`; nobody copies or rewrites it. |
| Size | A change summary, not bulk content: it becomes one reliable frame on each attached connection (§5), and a frame over the connection's limit closes that connection like any oversized notification. |

## 4. Emitting

```rust
/// Announces a committed change of process-level state about one Session.
pub fn notify(ctx: &Ctx, session_id: &str, notice: SessionNotice);
```

| Rule | Content |
| --- | --- |
| When | After the change commits, with the owner's own context. A change that fails or is abandoned sends nothing. |
| Call | Synchronous: a rutis `emit` that neither waits for listeners nor reports what they did. An emission the bus rejects is logged as a warning, and the commit stands. |
| Delivery | Decided by who listens at emission. A Session that is not loaded, or that no client observes, has no listener for the notice, so it is dropped. Nothing is queued for a later load or subscriber. |
| Dependencies | None. `notify` is a function on the shared bus: the owner declares no dependency on SessionBroadcast, Rpc, Agents or any optional plugin and needs no lifecycle hook, and it behaves the same whether they are present, absent or reloading ([ADR 002](../../adr/002-plugin-scope.md) D3). |
| Order | Each Session has one channel, so every listener receives notices and `Event` records in emission order. An owner that reacts to an event it observed through the broadcast emits after that event's record, so clients see the notice after the event. Concurrent changes reach listeners in `notify` order: an owner whose clients need commit order calls `notify` inside its commit serialization, or relies on its revision. |
| Reload | While an owner unloads or reloads it emits nothing, and nothing is replayed when it returns. A reload of the Session's capabilities does not affect `notify`, which does not pass through the SessionBroadcast plugin. |
| Non-Rust owners | No binding. SessionStore, the emitter SessionTitle needs, is Rust. ExternalAgent ([#145](https://github.com/arcships/dim/issues/145), P3, TypeScript first) emits `externalAgent.activity` notices from its TypeScript process row, the owner of external members' activity; the binding it needs is raised in [ExternalAgent §9.1](../plugins/ExternalAgent.md#91-typescript-binding-of-session-notices) and is not accepted yet. |

SessionStore's `setTitle` (S5b), for example, commits the new revision and then calls
`notify(&ctx, session_id, SessionNotice { source: "SessionStore".into(), kind: "title".into(), data })` inside its
serialization of title writes, where `data` is `{title, titleSource, titleRevision}`
([SessionStore](../plugins/SessionStore.md#operations) and [S5b §7](session-title-metadata.md#7-the-title-notice)).

## 5. Rpc forwarding

| Rule | Content |
| --- | --- |
| Wire | Notification `session.notice` with params `{sessionId, source, kind, data}`. `source`, `kind` and `data` are the record's, unchanged. `sessionId` is the subscription's Session, the routing field that every Session notification carries. |
| Protocol | Protocol v3 connections only; v1 and v2 never receive it, because their notification sets are frozen. The protocol version stays 3: the notification is additive, like the event kinds v3 has gained, and clients ignore notifications and kinds they do not handle. The CLI ignores it, and `exec --json` prints it with the Session's other notifications. `hello.features` lists only methods, so it does not list this notification. |
| Hello | A connection's protocol version, which decides notice eligibility, is published only after its `hello` reply has entered the connection's ordered outbound queue; a v3 notice can therefore neither precede the `hello` reply nor reach a v2 connection. |
| Path | The subscription's existing broadcast listener projects the record, so on each connection a notice keeps its channel order relative to `session.event`. It is queued like every Session notification on the connection's reliable queue: a client that cannot keep up is disconnected as a slow consumer, never left silently stale. |
| Window | From the attach that registers the listener until the subscription ends: detach, replacement, connection close or `Closed`. A notice emitted before attach is not replayed. A client that attaches or re-attaches reads current state through the owner's query (`session.summaries` for the title) after the attach, and every later change arrives as a notice. A change can show up in both; the owner's revision deduplicates it. |
| Close | The channel's `Closed` record ends what the subscription forwards. A notice that races the close and follows `Closed` is dropped, so `session.closed` stays the subscription's last Session notification. The owner's query reads the state afterwards. |

## 6. C4 leaf observers

A C4 leaf row listening to `session/{sessionId}` receives every record of the channel. A notice arrives as
`{kind: 'notice', notice: {source, kind, data}}`, and an observer skips it as it skips `fence` and `closed`
([C4 broadcast contract](../session-typescript.md#native-session-broadcast)). Notices report process
state whose owners already provide queries, and no TypeScript Session part consumes one: SessionTitle observes
`input/applied` and reads title metadata from SessionStore. A future consumer would add its own leaf contract.

## 7. Timing and conflicts

| Scenario | Outcome |
| --- | --- |
| Session not loaded | No listener: dropped. The next attach reads current state |
| Session loaded, no client attached | Dropped; a C4 leaf row listening to the channel skips it |
| Notice emitted before attach | Not replayed |
| Detach, or an attach that replaces the subscription | The old subscription's listener is gone; the new one receives later notices only |
| Session closing | Forwarded before `Closed`, dropped after it |
| Owner reload | Nothing emitted while the owner is not running; nothing replayed |
| Slow client | Disconnected as a slow consumer, as for events |
| Protocol v1 or v2 connection | Never receives notices |
| Changes from two owners, or two changes from one owner | Emission order on the channel |

## 8. Rule check

| Rule | How this design complies |
| --- | --- |
| [Principles](../../architecture/rust-agent/principles.md) §4 rule 1 (command events) | Not applicable: a notice changes no state; owners change their own state through their typed operations. |
| §4 rule 2 (handover) | Not applicable: nothing is handed over. |
| §4 rule 3 (before callbacks) | Not applicable: no before callback. |
| §4 rule 4 (no business meaning in core) | The record carries no business meaning: the broadcast and Rpc read none of its fields and name no owner. |
| §4 rule 5 (batches) | Not applicable: notices are not Timeline events and have no batches. |
| §4 rule 6 (model context) | No model-visible effect. |
| §4 rule 7 (public events and services) | Owners use the public channel key through `notify`; Rpc and clients see only the record. |
| §4 rule 8 (infrastructure unchanged) | Answers SessionTitle §9 R1 inside the broadcast: one record and one emission function, verified by broadcast-to-client flow tests. Timeline, Dispatcher, facades and assembly do not change. |
| §4 rule 9 (only decision facts in the Timeline) | Notices exist so that process-level state stays out of the Timeline. |
| §1 (Capability and App Server boundary) | The record and `notify` live with the broadcast in `dim-agent-capabilities`; the wire projection lives in Rpc. |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | A notice concerns one Session and travels on its channel; emitters keep their own scope. |
| ADR 002 D2 (no wiring modules) | No wiring module or registry. |
| ADR 002 D3 (dependencies follow scope) | `notify` needs no dependency; Rpc depends on no owner. |
| ADR 002 D4 (configuration) | Not applicable: no configuration. |
| ADR 002 D5 (language-neutral contracts) | The record and the wire form are data, `{source, kind, data}`; §3 and §4 give the Rust binding. |

Requirements raised to mechanism owners: none.

## 9. Acceptance

Flow tests in [notices.rs](../../../crates/dim-app-server/tests/transport_flow/notices.rs) run over IPC and
WebSocket:

1. A Session-scoped test owner observes `input/applied` through the broadcast and calls `notify`. Each of two
   attached v3 clients receives one `session.notice`, with `source`, `kind` and `data` unchanged and after that
   event's `session.event`. An attached v2 client receives the same events and no notice.
2. A process-level stand-in for an owner (`RunningServer::notify`, feature `testing`) emits notices. The attached
   client receives them. A notice emitted while the client is detached or the Session is unloaded is never
   delivered, and a re-attached client receives only notices emitted after the attach. A notice emitted after the
   `session.close` reply, and so after `Closed`, does not reach the client before `session.closed`.

## 10. Decisions

| # | Decision | Reason |
| --- | --- | --- |
| D1 | `Notice` is a record on the existing Session channel | One channel keeps notices in order with events and reuses Rpc's listener; a separate channel would need a second subscription and lose that order |
| D2 | `notify` is a function, not a service or plugin | Owners must not depend on optional plugins, and a notice needs no lifecycle: an owner's reload only stops its notices |
| D3 | No listener means dropped; nothing is queued or replayed | R1 requires it; the owner's query is the recovery, and a queue would need retention and its own lifecycle |
| D4 | The record does not name the Session; Rpc adds `sessionId` | The channel identifies the Session, as for `Event`; every Session notification routes on `sessionId`, and an owner cannot mislabel it. This refines R1, whose wire form is the bare record: the title data's own `sessionId` becomes redundant, and S5b drops it ([S5b §7](session-title-metadata.md#7-the-title-notice)) |
| D5 | Protocol v3 only, no version bump | v1 and v2 are frozen; the notification is additive, like v3's new event kinds |
| D6 | Reliable delivery: a slow client is disconnected | Events behave the same way; a silently dropped notice would leave the client stale with no signal |
| D7 | Notices after `Closed` are dropped | `session.closed` stays the subscription's last Session notification; the owner's query covers the gap |
| D8 | C4 leaf parts skip notices | A row listening to the channel receives them; no TypeScript consumer needs them |
| D9 | The body is shared through `Arc` | No per-listener copy, as for `Event` bodies |
