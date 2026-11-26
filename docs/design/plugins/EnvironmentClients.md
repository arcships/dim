# EnvironmentClients

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1019](https://github.com/arcships/dim/issues/1019). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentClients tells the model which client connections are attached to its Session. It is a read projection of the App Server's connection and UI registries; it owns no connection state. The snapshot is appended directly to the branch input. It is not a Prompt source and has no version, keyed state or compaction replay.

## 1. Decisions

- One snapshot at branch open, and one snapshot per observed connection change.
- The branch-open snapshot is a `BranchLifecycle::register_opening` receipt, not work the published branch still owes: the opening waits for the snapshot and for the `input/applied` the input queue writes for it.
- The snapshot is an `input/message` event with an Append-mode Raw block and no key.
- No `PromptRegistry`, `LoadPrompt`, `ConfigureControl`, version or replay.
- The projection is Session-level. It does not say which branch a client views.
- Per-message input origin is deferred; repeating an origin on every message is too redundant.
- The body lists connections, not people. A claimed label is not an authenticated identity.
- Only the matching Session receives the snapshot; no broadcast.
- The shared `dim-state` guide entry is registered by `PromptGuide` ([Prompt](Prompt.md#4-the-built-in-entries-and-their-writers)); this part does not register a second entry.

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

sessionClientsHost.presences {sessionId} -> {presences: [ClientPresence]}
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

A UI leaf and its own protocol connection produce one `tui` entry, never an additional `cli` entry. Deduplication uses the App Server's connection identity, not the claimed label. Two distinct attached connections remain two entries even if their channel and label are identical. The TUI's `ui.open` caller retains that same connection for `ui.attach` and Session operations ([Tui §3.6](Tui.md#36-ui-registry-and-ui-leaves)).

An ACP gateway is one connection. Several logical clients behind the gateway are not distinguished; the label, if any, is the gateway's.

## 5. Trigger and append

### Branch open

The branch part registers an opening receipt in `apply` when the branch is
`Loading`. The receipt reads the current presence list, appends the snapshot,
and returns once the branch dispatched it: the input queue has written the
snapshot's `input/applied` by then, so the branch's opening seals with both
committed and dispatched. `session.create` and a restore therefore return with
the snapshot in the branch's published history, and the first request carries
it. The presence list comes from the App Server's in-memory registry, so the
receipt awaits no I/O of its own.

A child line's own history starts with TimelineTree's identity input, which the
tree writes after the branch opening seals. The receipt therefore appends
nothing for such a line. It decides from the opening kind the line's owner
wrote — the root line's `branch/start`, a child line's `branch/start` naming the
parent it was cut from, or a stored line's `branch/resume` — which the tree
hands the opening receipts when it seals the opening. The receipt makes no
storage read, so the opening cannot be delayed by one and the decision does not
depend on whether the part mounted before the opening event was dispatched. The
input that opens the line stays its first input, and the first loop pass after
`BranchLifecycle::activated()` appends the snapshot, so it follows that input.

A branch whose opening sealed before the part mounted, such as a `Connection`
reload onto a live branch, registers nothing either; the first loop pass after
`BranchLifecycle::activated()` appends the snapshot there.

A receipt belongs to the generation that registered it. A generation that
unloads while the opening still collects receipts — a reload onto a loading
branch — leaves its receipt in the opening; the receipt still decides from the
opening kind its owner wrote, so a child line's identity input stays first, and
it checks that generation's cancellation token right before it appends. A
cancellation that lands after that check can leave
a second snapshot with the same source, as a reload does: both events stay in
the branch's history, and the later `<dim-state>` block of the same source
replaces the earlier one for the model ([Prompt](Prompt.md#4-the-built-in-entries-and-their-writers)).
The replacement generation registers its own receipt, and the line gets the
snapshot from that generation's first pass after activation, after its
identity input. The loop after activation checks the same token right before
its own append.

A snapshot appended after activation — a child line's first pass, a reloaded
part's first pass, or a `ClientsChanged` wake — is published before the branch
dispatches it, so a temporary route adoption that arrives before the branch
consumes it is refused with `provider_route_conflict`
([route admission](../appserver/provider-management.md#connection-scoped-provider-routes));
the same call is adopted once the branch has dispatched the snapshot and its
`input/applied`, so the caller retries the call or waits for `session.idle`
first.

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

No polling or broadcast. The snapshot uses the existing `input/message` event; no presence event type is added to the Timeline.

A change that arrives during a turn is appended when it arrives. The input-ordering consequence is tracked separately; this design does not work around it.

## 6. Plugin structure

- `ConnectionPlugin` provides `SessionClientsHost`.
- `ConnectionPlugin` emits `ClientsChanged` on attach, detach, UI attach/close/replace and connection finish.
- `ConnectionPlugin` registers one branch part: `EnvironmentClientsBranch`.
- The App Server profile requires `EnvironmentClientsBranch` (`AgentBuilder::require_part`), so a branch opening never seals before the part registered its opening receipt.
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
| Snapshot append fails during the opening | Log; the published branch's first loop pass appends it after activation |
| Snapshot append fails after activation | Log; do not mark the body as appended; a later wake can retry |
| Body unchanged | Append nothing |
| Branch closing | Append nothing |
| Label missing | Channel only |
| Remote connection | Not reported until a producer exists |

The in-memory presence read is infallible; it returns an empty list for an unknown Session.

## 9. Dependencies

| Dependency | Purpose |
| --- | --- |
| `SessionClientsHost` | Read the current connections |
| The branch's input path | Append the snapshot |
| `BranchLifecycle` | Register the opening receipt, which the opening hands the line's opening kind; wait for publication before appending afterwards; stop when the branch closes |
| `Dispatcher` | The opening receipt returns once the branch dispatched the snapshot |
| `Timeline` | Append the snapshot |
| Process bus (`ClientsChanged`) | Wake on a connection change |

The process `sessionClientsHost` is a data face with the request and reply defined in [service faces §4.9](../service-faces.md#49-旧主机端口credentialvariableshost-等六个). `sessionIdentity`, `timeline` and `branchLifecycle` are also data faces. This Rust projection still uses owner methods outside the dispatched operations and is not a leaf-ready foreign part.

No Prompt registry, no configure control, no version.

### Requirements raised to mechanism owners

None to the runtime or rutis.

## 10. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded; Capability | The connection registries stay with the App Server; `EnvironmentClientsBranch` is a Capability because it writes snapshot input facts through its branch Timeline. No new process part is added. |
| §4 rule 1 Command events | Presence changes no state. |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | The opening receipt appends before the branch is Active; later appends run in the part's own task and the event handler only wakes it. |
| §4 rule 4 Core carries no business meaning | The body is the App Server projection; no core branch. |
| §4 rule 5 Batch | A snapshot is one branch input. |
| §4 rule 6 Context from consumed events | The snapshot is an input; no second context path. |
| §4 rule 7 Public services | `SessionClientsHost` is the public read port. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade contract change; the part reads the existing `branchLifecycle` and `dispatcher` faces. |
| §4 rule 9 Timeline holds decision facts | The delivered snapshot is an input fact; live presence remains in the App Server registries. |
| ADR 002 D1 | Connections stay with the App Server; the branch part only projects them. |
| ADR 002 D2 | No adapter plugin; the projection is the capability's own purpose. |
| ADR 002 D3 | The branch part depends on the process port; no core depends on the capability. |
| ADR 002 D4 | No new configuration. |
| ADR 002 D5 | The body is data; Rust is a binding. |

## 11. Verification

| Scenario | Expected |
| --- | --- |
| Branch open | The snapshot and its `input/applied` are committed and dispatched before the branch is Active, so `session.create` and a restore return with both |
| A child line opens | The identity input is the line's first input; the snapshot follows it once the line is published |
| A temporary route adopted with no `session.idle` after create | Adopted; the opening left nothing undispatched |
| A temporary route adopted while a snapshot the part appended after activation is still dispatched | Refused with `provider_route_conflict`; the same call is adopted once the branch dispatches the snapshot and its `input/applied`; the caller retries or waits for `session.idle` |
| Opening sealed before the part mounted | The snapshot is appended after activation |
| The part is replaced while its line loads | The retired receipt sees the opening kind and writes nothing; the replacement appends the line's one presence snapshot after its identity input |
| A generation is cancelled before its receipt's append | The receipt checks the token right before it appends and appends nothing |
| A generation is cancelled after its receipt's last check | It may append one more snapshot, as a reload does: both events stay in history, and the later `<dim-state>` block of the same source replaces the earlier one for the model |
| A part mounts after the line's `branch/start` was dispatched | It decides from the opening kind the tree hands the receipt: the receipt writes nothing ahead of the identity input, and the snapshot follows once the line activates |
| The Timeline store stalls during the opening | The receipt makes no read of it: the branch opens within its budget, and a child line's identity input stays first |
| Two TUI leaves | Two `tui` entries, not one count |
| A TUI leaf and its protocol connection | One `tui` entry; no duplicate `cli` entry |
| Two distinct connections with the same label | Two entries; labels are not identities |
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
3. The `desktop` client-name convention and the `unknown` fallback.
4. The `remote` channel when RemoteGateway P3 lands.
5. Per-message input origin is deferred.
