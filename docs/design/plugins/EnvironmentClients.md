# EnvironmentClients

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1019](https://github.com/arcships/dim/issues/1019). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentClients tells the model who is attached to its Session: how many clients, by channel, and (only if later promoted) whether anyone is viewing the current line. It is a read projection of the App Server's connection and UI registries; it owns no connection state.

## 1. Scope and non-goals

Owns:

- the process host contract `sessionClientsHost` that answers a per-Session client count;
- the prompt source `dim/environment/clients` and its body;
- the branch trigger that adopts the source when the operator's input arrives, and at branch open.

Does not own:

- connection lifecycle, attach/detach, UI leaf lifecycle or the `ui.*` methods; those stay in Connection and UiHost;
- the client view (line); that is [ClientView](ClientView.md);
- client capabilities; that is a later design;
- approvals, questions, `interactive` or permission.

## 2. Own contract

### 2.1 The `sessionClientsHost` host

| Element | Form |
| --- | --- |
| Service | `sessionClientsHost`, process scope, read-only |
| Method | `counts {sessionId}` → `{total, byChannel}` |
| `total` | Number of attached clients, one per client, not per connection |
| `byChannel` | Map from channel name to count, sorted by channel |
| Channels | `tui`, `cli`, `web`, `acp`, `remote`, `desktop`, `unknown` |
| Errors | None for a read; an unloaded App Server means the service is absent |
| Cancellation | None; the read is a snapshot of in-memory registries |

A client is one attached connection, except that a UI leaf and the connection its `Tui` uses are one `tui` client. The count is computed on demand from the existing connection and UI registries; there is no second registry and no attach/detach bookkeeping.

### 2.2 Channel mapping

| Condition | Channel |
| --- | --- |
| The connection is the connection of a registered UI leaf (`UiHost` instance) | `tui` |
| `hello.clientKind = cli` and no UI leaf | `cli` |
| `hello.clientKind = web` | `web` |
| `hello.clientKind = other` and the client name is the ACP gateway | `acp` |
| `hello.clientKind = other` with a recognized desktop client name | `desktop` |
| A paired remote device connection (P3) | `remote` |
| Anything else | `unknown` |

`hello.clientKind` and `hello.clientName` are stored on the connection at hello. They are informational; the channel is not an authorization and not a durable identity. A UI leaf wins over its connection's `cli` kind.

### 2.3 The prompt source

| Field | Value |
| --- | --- |
| Name | `dim/environment/clients` |
| Delivery | `State` (`<dim-state>`) |
| Order | `230`, after the date (`210`) and before Skills (`300`) |
| Author | EnvironmentClients |
| Class | Fact |
| Lifecycle | Registered by the Session part from `apply`, withdrawn with it |

The body is one of:

```text
No client is attached.
```

```text
1 client attached: 1 tui.
```

```text
2 clients attached: 1 tui, 1 web.
```

The count is by channel, sorted by channel name. The body carries no label, client name, connection id, `uiId`, attach time, revision or "last departure". If a client view is later promoted, the same body appends one sentence: `1 client is viewing this line.` or `No client is viewing this line.` ([ClientView](ClientView.md)).

The body is final text. It is generated from the counts only; no external content is interpolated, so no escaping is needed today. If a client name is added later, the owner escapes it with `context_block` and bounds its length.

### 2.4 Adoption

- The Session part registers the source; the initial configuration adopts it at branch open.
- The branch part follows `turn/start` on the branch's Dispatcher. For each delivered event it spawns a tracked task that asks Prompt to adopt the source through `ConfigureControl::configure(targets([SOURCE]))`; the handler returns without waiting. The tasks are closed with the branch.
- The branch part does not listen to attach/detach directly; idle churn produces no block. The next turn start reads the current snapshot.
- The state block is written during the turn, before or after that turn's first request; the design accepts this one-request lag unless a request-boundary check is added. Prompt refuses an adoption after the branch closes, so no write follows close.

## 3. Events and state

No Timeline event is written or consumed. Presence is process state: connections, UI instances and their counts. It is read when the source is loaded and when a branch adopts; it is not persisted and not replayed.

A restored branch loads the source at `branch/resume`; the body is the current count, not the count at the cut. A closing branch adopts nothing.

## 4. Dependencies

| Service | Purpose |
| --- | --- |
| `P:SessionClientsHost` | The App Server read-only counts |
| `S:SessionIdentity` | The Session id the source reads |
| `S:PromptRegistry` | Register the source |
| `B:Dispatcher` | Follow turn starts |
| `B:dyn ConfigureControl` | Adopt the source |

The feature is installed only by an assembly that provides `SessionClientsHost` (the App Server daemon profile). A native/direct profile installs neither the host nor the parts; the model sees no presence block.

## 5. Absence, errors and empty

| Situation | Result |
| --- | --- |
| No `SessionClientsHost` | The feature is not installed; no source, no block |
| Counts are zero | `No client is attached.` (a fact, not `cleared`) |
| The host is being reloaded | No new block; the last adopted block stays |
| The branch is closing | No adoption |
| A leaf is `starting` and has not attached | Not counted as a viewer yet; the caller connection that opened it is counted under its own channel. Whether the caller remains a separate client after the leaf attaches is an open decision (§10) |

`cleared="true"` is never used for presence. It would say the state no longer applies; an empty client set is a real situation.

## 6. Noise and identity

- One block per turn at most, and only when the count changed.
- Per-branch local copies are expected: every open line has its own branch part and its own block.
- No client roster, labels, ids, times or departure history in the model body. Management/UI may read more through its own surface.
- No token, device id or device name. The remote channel is named only as `remote`.
- A client's `hello` identity is not durable; after a daemon restart, reconnections are new clients.

## 7. Cross-module requirements

### 7.1 App Server

- Store `hello.clientKind` and `hello.clientName` on the connection at hello.
- Add a per-Session count query to the existing connection registry, using the UI registry to mark a leaf's connection as `tui`.
- Provide `SessionClientsHost` from the process part that already owns the connection hub; no new plugin and no second registry.
- Keep the count out of `SessionInfo`; the model-facing projection is the state source. A management read may reuse the same query later.

### 7.2 RemoteGateway

Its document says the model never sees device ids, names, tokens, entry names or diagnostics. A `remote` channel count violates none of that; the owner updates the sentence to allow the channel label and confirms it.

### 7.3 Clients

No client change is required for the count. Line view is [ClientView](ClientView.md).

## 8. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded | Session part registers the source; branch part adopts; the App Server host is process state. No wiring module. |
| §4 rule 1 Command events | No other module's state is changed. |
| §4 rule 2 Handover | No handover; presence does not control a turn. |
| §4 rule 3 Before callbacks | None. The Dispatcher handler spawns a tracked adoption and returns. |
| §4 rule 4 Core carries no business meaning | The host returns generic counts; the source owns the wording. |
| §4 rule 5 Batch | No new facts. |
| §4 rule 6 Context from consumed events | The block is an adopted input; no second path. |
| §4 rule 7 Public services | `sessionClientsHost` is a named process service; the source is a registered prompt source. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Presence is process state; it does not enter the Timeline. |
| §4 rule 10 Adopted inputs | The body reaches the model only through Prompt's state input. |
| ADR 002 D1 | The fact belongs to the Session; the branch part only adopts. |
| ADR 002 D2 | No adapter plugin; the App Server host is its own registry's read. |
| ADR 002 D3 | The Session part depends on the process host; no core depends on the feature. |
| ADR 002 D4 | No configuration is added. |
| ADR 002 D5 | The service and count shape are data; Rust is a binding. |

## 9. Verification

| Scenario | Expected |
| --- | --- |
| Two TUI leaves attach the same Session | At the next turn start: `2 clients attached: 2 tui.`; no attach blocks |
| A TUI leaf's underlying connection also attaches | It is one `tui`, not `tui` + `cli` |
| A line client attaches | `cli` |
| Web attaches | `web` |
| ACP attaches | `acp` |
| A paired remote device attaches | `remote`; no device id/name |
| A leaf is `starting` | Not counted until it attaches |
| One of two clients leaves while idle | No block; the next turn start says one |
| Last client leaves | `No client is attached.` |
| No provider | No block, no error |
| Restore | Current count, not an old count |
| Compaction | The latest presence block is replayed; old ones are gone |
| Closing branch | No adoption, no write |

## 10. Open decisions

1. The exact wording of the non-interactive case and whether a Collaboration member names its counterpart.
2. Whether management/UI exposes the full per-client list or only the counts; the model body stays minimal either way.
3. The `desktop` client-name convention and whether an unknown `other` client should be `desktop` or `unknown`.
4. The remote channel's implementation when RemoteGateway P3 lands.
5. Whether the count is read at the request boundary (#986) to remove the one-request lag.
6. Whether the `ui.open` caller remains a separate client after its leaf attaches, or the two are one `tui`.
