# EnvironmentClients

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1019](https://github.com/arcships/dim/issues/1019). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentClients tells the model which client connections are attached to its Session. It is a read projection of the App Server's connection and UI registries; it owns no connection state. The snapshot is appended directly to the branch input. It is not a Prompt source and has no version, keyed state or compaction replay.

## 1. Decisions

- One snapshot at branch open, and one snapshot per observed connection change.
- The snapshot is an `input/append` Raw block.
- No `PromptRegistry`, `LoadPrompt`, `ConfigureControl`, version or replay.
- The projection is Session-level. It does not say which branch a client views.
- Per-message input origin is deferred; repeating an origin on every message is too redundant.
- The body lists connections, not people. A claimed label is not an authenticated identity.
- Only the matching Session receives the snapshot; no broadcast.

## 2. Snapshot body

No connection:

```text
<dim-state source="dim/environment/clients">No client connection is attached.</dim-state>
```

One connection:

```text
<dim-state source="dim/environment/clients">1 client connection attached:
- tui "eric@laptop" (claimed)</dim-state>
```

Several connections:

```text
<dim-state source="dim/environment/clients">3 client connections attached:
- tui "eric@laptop" (claimed)
- tui
- web "alice" (claimed)</dim-state>
```

Rules:

- One line per connection. Do not collapse TUI connections into a channel count.
- A missing label leaves the channel alone; no empty quotes.
- `(claimed)` marks a self-declared `clientName`; it is not an identity.
- Escape quotes, backslashes and control characters; cap a label at 64 characters.
- The body is sorted by channel and label, so the same audience produces the same text.
- A label is escaped as tag text; `<`, `>` and ampersands cannot break the `<dim-state>` element.
- The body states connections; it does not claim how many people are present.
- No ids, device names, entry names, attach times or departure history.

## 3. Data

The App Server stores, per connection:

```text
ClientIdentity {
  channel: ClientChannel,
  label: Option<String>,
}
```

The process port returns the current list:

```text
ClientPresence {
  channel: ClientChannel,
  label: Option<String>,
}

SessionClientsHost::presences(session_id) -> Vec<ClientPresence>
```

A UI leaf takes the label of the connection that hosts it; without one, only the channel is present.

## 4. Channels

| Channel | Source |
| --- | --- |
| `tui` | A registered UI leaf |
| `cli` | `hello.clientKind = cli` |
| `web` | `hello.clientKind = web` |
| `acp` | `other` + `clientName = AcpGateway` |
| `desktop` | `other` + `clientName = Desktop` or `dim-desktop` |
| `unknown` | Another `other`, or before `hello` |
| `remote` | Reserved; no producer yet, not reported |

A UI leaf is not counted again as `cli`. Its underlying line connection may also be attached and counted as `cli`; the two entries are kept, because the body counts connections.

An ACP gateway is one connection. Several logical clients behind the gateway are not distinguished; the label, if any, is the gateway's.

## 5. Trigger and append

### Branch open

The branch part appends the current snapshot once after publication. The first request may race that append; a later request in the batch carries the value.

### Connection change

App Server emits:

```text
ClientsChanged { session_id }
```

The branch part:

- ignores events for another Session;
- reads the current presence list;
- builds the body;
- appends only when the body differs from the last appended body.

No polling. No Timeline event. No broadcast.

A change that arrives during a turn is appended when it arrives. The input-ordering consequence is tracked separately; this design does not work around it.

## 6. Plugin structure

- `ConnectionPlugin` provides `SessionClientsHost`.
- `ConnectionPlugin` emits `ClientsChanged` on attach, detach, UI attach/close/replace and connection finish.
- `ConnectionPlugin` registers one branch part: `EnvironmentClientsBranch`.
- There is no Session part for this projection.
- No new process plugin.

## 7. Privacy

- The model sees channel and an optional claimed label only.
- No client id, device id, token, entry name or connection id.
- A label is untrusted text: escaped and bounded.
- Presence grants no permission and changes no approval.

## 8. Errors and absence

| Situation | Result |
| --- | --- |
| No connection | One snapshot saying so |
| Presence read fails | Log; append nothing; the earlier snapshot stays |
| Body unchanged | Append nothing |
| Branch closing | Append nothing |
| Label missing | Channel only |
| Remote connection | Not reported until a producer exists |

## 9. Dependencies

| Dependency | Purpose |
| --- | --- |
| `SessionClientsHost` | Read the current connections |
| The branch's input path | Append the snapshot |
| Process bus (`ClientsChanged`) | Wake on a connection change |

No Prompt registry, no configure control, no version.

### Requirements raised to mechanism owners

None to the runtime or rutis.

## 10. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded; Capability | The connection registries stay with the App Server; `EnvironmentClientsBranch` is the Capability, and there is no process part. |
| §4 rule 1 Command events | Presence changes no state. |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | The branch part appends in its own task; the event handler only wakes it. |
| §4 rule 4 Core carries no business meaning | The body is the App Server projection; no core branch. |
| §4 rule 5 Batch | A snapshot is one branch input. |
| §4 rule 6 Context from consumed events | The snapshot is an input; no second context path. |
| §4 rule 7 Public services | `SessionClientsHost` is the public read port. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Presence is not a Timeline fact. |
| §4 rule 10 Adopted inputs | The model reads the appended input directly. |
| ADR 002 D1 | Connections stay with the App Server; the branch part only projects them. |
| ADR 002 D2 | No adapter plugin; the projection is the capability's own purpose. |
| ADR 002 D3 | The branch part depends on the process port; no core depends on the capability. |
| ADR 002 D4 | No new configuration. |
| ADR 002 D5 | The body is data; Rust is a binding. |

## 11. Verification

| Scenario | Expected |
| --- | --- |
| Branch open with one TUI | One snapshot with one `tui` entry |
| Two TUI leaves | Two `tui` entries, not one count |
| A TUI leaf and its line connection | Both entries; the body says connections |
| Web attach then detach | Two snapshots: with and without the web entry |
| Label with quotes/control characters | Escaped and bounded |
| Same presence read twice | One snapshot; no duplicate |
| Event for another Session | No append |
| Idle Session | No polling block |
| Remote connection | Not reported |
| Compaction | The snapshot may be removed; no replay |

## 12. Open decisions

1. Whether the body lists every connection or caps a long list.
2. Whether a claimed label is shown by default or only when the client opts in.
3. Whether the `ui.open` caller and its leaf are one connection or two.
4. The `desktop` client-name convention and the `unknown` fallback.
5. The `remote` channel when RemoteGateway P3 lands.
6. Per-message input origin is deferred.
