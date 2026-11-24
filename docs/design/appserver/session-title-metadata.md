# SessionStore title metadata

The [SessionStore contract](../plugins/SessionStore.md) supersedes the title interface: `store.title` reads the title with its source and revision, and `store.setTitle(id, change)` takes the caller's change, serializes the write, commits the returned title and announces it. The revision is the owner's; the interface has no `expectedRevision`. The data, notice and list paragraphs below describe the stored title and remain in force.

Status: Implemented in roadmap item S5b ([#60](https://github.com/arcships/dim/issues/60),
[#96](https://github.com/arcships/dim/pull/96)), after C6 ([#49](https://github.com/arcships/dim/issues/49),
[#85](https://github.com/arcships/dim/pull/85)); the implementation follows this design. Review refined when a write
is admitted and how unload settles it (§6, D14). Requirement: [SessionTitle](../plugins/SessionTitle.md) §2, §4 and
[A.2](../plugins/SessionTitle.md#a2-depended-on-shapes-owners-define-them). Overview:
[Appendix B.2](../overview.md#b2-app-server-process-plugins), SessionStore and Rpc rows. Scope:
[ADR 002](../../adr/002-plugin-scope.md) process scope, inside the existing SessionStore plugin, with three Rpc list
fields. Consumer: SessionTitle.

## 1. Summary

The Write and Binding bullets are superseded by [the SessionStore contract](../plugins/SessionStore.md#operations); the Data, Notice, Lists, Lifecycle and Format bullets remain in force.

SessionStore keeps one title per Session with a per-Session revision. The title changes only through `setTitle`, whose
change decides the next title under the owner's serialization. After each committed change SessionStore announces the new title on the Session's broadcast
channel ([C6](session-notices.md)), and Rpc shows the title in protocol v3 Session lists.

- **Data.** A revision counter (0 means never written) and either no title or a title `{text, source}` with source
  `auto` or `manual`. Clearing keeps the counter, so a revision never repeats.
- **Notice.** After each commit, `{source: "SessionStore", kind: "title", data: {title, titleSource,
  titleRevision}}`, in commit order.
- **Lists.** Protocol v3 `session.summaries` entries carry `title`, `titleSource` and
  `titleRevision`.
- **Lifecycle.** A delete removes the title in S5c's delete transaction; a fork does not copy it (SessionTitle A7);
  a restart keeps it. Reading or writing a title never loads the Session.
- **Format.** Native store v16: one new table, upgraded in place from v15, and from v13 through v15's step.

No Timeline event, no Dispatcher, InputQueue, runtime or rutis change, no new plugin, no client method and no
protocol version.

## 2. Structure

```text
 Session scope                          Process scope (control node)
 ----------------------------           --------------------------------------------------------------
 SessionTitle (Session part)          SessionStore (existing plugin)
   inject 'store'                         store (HostDispatch)
     title ------------------------->       title    -> metadata()   ------------------+
     setTitle ---------------------->       setTitle -> set_title()                    |
                                                          |                            v
                                          title writer: one write at a time ---> SqliteStorage
                                            commit, then notify                   table session_metadata
                                              |                                        ^
 Session channel session/<id>  <--------------+  Notice {SessionStore, title, data}     |
   |                                                                                   |
   v                                    Rpc: session.summaries ------------------------+
 Rpc subscription (v3)                    SessionStore's catalog query joins the title row
   -> session.notice {sessionId, source, kind, data}
```

| Part | Scope | Change |
| --- | --- | --- |
| SessionStore ([timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs), [metadata.rs](../../../crates/dim-app-server/src/storage/metadata.rs)) | Process | `title` and `set_title`, the title writer and the `store` face; delete and creation rollback remove the title row; the catalog query reads it |
| SqliteStorage ([sqlite.rs](../../../crates/dim-app-server/src/storage/sqlite.rs)) | Process | v16 schema and its upgrade step |
| Rpc ([protocol.rs](../../../crates/dim-app-server/src/protocol.rs), [connection.rs](../../../crates/dim-app-server/src/connection.rs)) | Process | The v3 list fields |
| Service catalog ([lib.rs](../../../crates/dim-app-server/src/lib.rs)) | Process | the `store` face, registered once for Rust and the wire |
| Session broadcast, Rpc notice forwarding | Session, process | Unchanged (C6) |

## 3. Data and revisions

| Item | Meaning |
| --- | --- |
| `revision` | Per-Session counter of committed `setTitle` writes. 0 means never written. Each committed write adds one: a new title, the same text again, a source change or a clear |
| `title` | `{text, source}` or none |
| `source` | `auto` (SessionTitle's generation) or `manual` (`session.rename`). SessionStore stores the source it is given and attaches no rule to it. "A manual title always wins" is SessionTitle's rule, decided inside its change ([SessionTitle §4](../plugins/SessionTitle.md#4-timing-and-conflicts)) |

- **A clear keeps the counter.** `title: null` leaves the Session's row with no title and the new revision. Were
  the row deleted, the counter would restart at 0, and a generation that read revision 0 before a rename and a clear
  would then commit an automatic title over the user's clear.
- **Text as given.** SessionStore stores the text it receives. Title rules (trimming, length, a single line) are
  SessionTitle's ([A.3](../plugins/SessionTitle.md#a3-configuration)): its method validates rename input and
  normalizes generated text. The producer guarantees the contract, so SessionStore checks it again nowhere
  (AGENTS.md).
- **Per Session.** Revisions of different Sessions are independent. A fork starts at 0.

## 4. Operations

### 4.1 Language-neutral (ADR 002 D5)
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).

### 4.2 Rust
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).

## 5. setTitle sequence
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).

## 6. Handover, cancellation and settlement
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).

## 7. The title notice

| Field | Value |
| --- | --- |
| `source` | `"SessionStore"` |
| `kind` | `"title"` |
| `data` | `{title: string \| null, titleSource: "auto" \| "manual" \| null, titleRevision}` |

- **Once per commit.** Emitted after each committed write with SessionStore's own context through
  `session_broadcast::notify`. A rejected change, a missing Session or a failed write emits nothing.
- **No `sessionId` in `data`.** The channel names the Session, and Rpc adds `sessionId` to `session.notice`
  ([C6 D4](session-notices.md#10-decisions)).
- **The list's fields.** `data` holds exactly the three title fields of a v3 Session list entry (§8). A client
  applies a notice by copying them into its entry when `titleRevision` is higher than the entry's. A clear sends
  explicit nulls, so copying clears the entry.
- **Order.** Notices of one Session follow commit order (§5). `titleRevision` also lets a client discard anything
  older than what it holds.
- **Delivery** is C6's: dropped when nobody listens (the Session is not loaded, or no v3 client is attached), never
  replayed, and never sent to protocol v1 or v2.

## 8. Session lists

Protocol v3 `session.summaries` entries gain three fields:

| Field | Value |
| --- | --- |
| `title` | The title text, or `null` |
| `titleSource` | `"auto"`, `"manual"`, or `null` when there is no title |
| `titleRevision` | The revision the entry was read at; 0 when never written |

- SessionStore's catalog query joins the title row in the same SQL read, so a list costs no read per Session. A
  daemon without a native store has no titles; its v3 entries show `null`, `null`, `0`.
- v1 and v2 clients get no Session listing and never receive title notices
  ([C6 D5](session-notices.md#10-decisions)).
- **Why the revision is listed.** A list can be read before a commit and still reach the client after that commit's
  notice, on the same connection. The client keeps whichever has the higher `titleRevision`. C6 relies on this:
  "the owner's revision deduplicates it" ([C6 §5](session-notices.md#5-rpc-forwarding)).
- A client that attaches reads the list after the attach; every later change arrives as a notice (C6 §5).

Rpc never calls `setTitle` and gains no method. How each error reaches clients:

| SessionStore | `session.summaries` | `session.rename` ([SessionTitle A.1](../plugins/SessionTitle.md#a1-sessionrename)) |
| --- | --- | --- |
| `storage` | The catalog read fails as it does today | `storage_failed` |
| `not_found` | The Session is not listed | `session_not_found` |
| `rejected` | — | SessionTitle's change refused (for example a manual title exists); it discards its attempt ([§3.2](../plugins/SessionTitle.md#32-manual-rename)) |
| `closed` | — | `service_reloading`; the call is not admitted and the caller retries on the replacement generation |

## 9. Lifecycle

| Event | Title |
| --- | --- |
| Session created | None, revision 0; no row is written |
| Load, unload, close, restart | Unchanged: the title is stored with the Session's rows, not with its instance. `title` and `setTitle` work on a Session that is not loaded; their notices are dropped, as nobody listens |
| `session.delete` (S5c) | Removed in S5c's single delete transaction, before the Session record. Afterwards `title` and `setTitle` fail `not_found` |
| Creation rollback | The same transaction as a delete; it removes a row that a racing write committed |
| `session.fork` (S5c) | Not copied ([SessionTitle A7](../plugins/SessionTitle.md#appendix-e-assumptions)): the fork starts untitled at revision 0 |
| SessionStore or SqliteStorage reload | Settles as in §6. Consumers of `store` reload with SessionStore ([ADR 002](../../adr/002-plugin-scope.md) D3), which SessionTitle §1 accepts. SessionStore reloads only with SqliteStorage, which has no dependencies |

## 10. Timing and conflicts
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).

## 11. Storage format

**Decision: version 16, one new table, an in-place upgrade from exactly v15. A v13 store goes through v15's step in
the same transaction.** Recorded in [native-storage-format.md](native-storage-format.md#s5b-v16-格式决定).

```sql
CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 16))
CREATE TABLE session_metadata (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id),
  revision INTEGER NOT NULL CHECK (revision > 0), title TEXT,
  title_source TEXT CHECK (title_source IN ('auto', 'manual')),
  CHECK ((title IS NULL) = (title_source IS NULL)))
```

```text
 agent_sessions (session_id PK) <---- session_metadata (session_id PK, FK)
                                        revision      > 0, +1 per committed setTitle
                                        title         NULL after a clear
                                        title_source  'auto' | 'manual'; NULL exactly when title is NULL
 no row: revision 0, no title
```

- **Why a new version.** Startup requires the exact SQL schema
  ([sqlite.rs](../../../crates/dim-app-server/src/storage/sqlite.rs)), so a v15 daemon refuses a store with an
  extra table. The format document keeps a version only when old readers interpret the new data; an old daemon that
  cannot open the store fails that condition.
- **Why a table and not existing rows.** `agent_sessions.config` is the immutable creation `SessionConfig`, which a
  fork copies whole: a title there would be copied against A7 and would turn a mutable value into configuration.
  `settings_documents` belongs to Settings. Columns added to `agent_sessions` change its stored SQL, so they need a
  new version as well, and they would mix a mutable value into the creation record.
- **Why one row per Session.** Only the current title and revision are read. No decision uses earlier titles.
- **Index.** The primary key serves `title`, `setTitle`, the list join and the delete. No other index.

| Store found | Action at startup, in the existing admission transaction (`IMMEDIATE`, before writable PRAGMAs) |
| --- | --- |
| Empty | Create the v16 schema with marker 16 |
| Exact v16 schema, marker 16 | Open |
| Exact v15 schema, marker 15 | Recreate `agent_store_format` with `CHECK (version = 16)`, insert 16, create `session_metadata`. No business row is read or rewritten; a failure rolls back to the untouched v15 store |
| Exact v13 schema, marker 13 | The same with S5a's step first: the v16 marker, S5a's two tables and index, then `session_metadata` |
| Anything else: a v15 schema with another marker, a partial upgrade, older formats, the branch-only v14, unknown schemas | Refuse and preserve, as today |

The upgrade is one-way and takes no backup, as decided for S5a during development. A v15 daemon refuses an upgraded
store with the existing "unsupported SQLite schema … preserved" error. The daemon writes
`Upgraded native store <path> from format v15 to v16; earlier dimcode versions cannot open it.` to stderr, once,
after the upgrade commits (S5a's log line, with the version the store came from).

## 12. TypeScript binding

| Item | Rule |
| --- | --- |
| Identity | `dyn HostDispatch` at `host_key("store")`, provided by SessionStore; `register_shared("store")` in App Server's service catalog, beside `sessions` and `settings` |
| Use | A C4 Session part declares `inject: ['store']` and calls `ctx.use('store')`, the path `settings` takes from a Session part ([settings-owner fixture](../../../runtime/fixtures/settings-owner.ts)). Types are in [npm/plugin.d.ts](../../../npm/plugin.d.ts) |
| Modes | `title` and `setTitle` are both `async` |
| Writer | The service carries no caller identity, as for `settings` (AGENTS.md rule 11). SessionTitle is the title's one writer by design ([SessionTitle §2](../plugins/SessionTitle.md#2-data-and-state)); other packages only read |
| Reload | A consuming row reloads with SessionStore. SessionStore reloads only with SqliteStorage, so no directory part that never reloads is needed, unlike `sessions`, whose index reloads with Settings ([sessions D1](sessions-service.md#3-decisions)) |
| Missing provider | A daemon without a native store has no SessionStore, so a row that injects `store` stays Pending under the existing rules; native Sessions continue |

This uses only the existing catalog and `HostDispatch` mechanisms ([#4](https://github.com/arcships/dim/issues/4)
named-service rules). S5b leaves nothing for C4 or S4 to add; S4's own named auxiliary binding is a separate
SessionTitle gate.

## 13. Changes to other documents

| Document | Change |
| --- | --- |
| [SessionTitle](../plugins/SessionTitle.md) | §0 metadata gate and handover paragraph, §1 part row, §6 interface rows, §9 R1 notice data, A.2, A.4 method names, Appendix F and G point to this contract |
| [Session broadcast notices](session-notices.md) | The first emitter's status, the title data shape, D4's outcome |
| [Overview B.2](../overview.md#b2-app-server-process-plugins) | Rpc and SessionStore rows |
| [Session deletion and fork](session-deletion-and-fork.md) | §3.3 and §14 point to this design |
| [Native storage format](native-storage-format.md) | The v16 decision, the table, the record example |
| [ADR 001](../../adr/001-harness-foundation.md), [Timeline storage](../../architecture/rust-agent/timeline-storage.md) | Current format v16 |
| [TypeScript plugin authoring](../../typescript-plugin-authoring.md), [npm/plugin.d.ts](../../../npm/plugin.d.ts) | The `store` service and its types |

## 14. Rule check

| Rule | How this design complies |
| --- | --- |
| [Principles](../../architecture/rust-agent/principles.md) §4 rule 1 (commands for others' state) | The title is SessionStore's process state, outside any Timeline. Others change it only through SessionStore's typed `setTitle`, and the change decides the next title while the owner serializes the writes |
| §4 rule 2 (handover) | The handover is explicit: the call (§6). Before it the caller can cancel; after it only SessionStore settles the write |
| §4 rule 3 (before callbacks) | Not applicable: no before callback |
| §4 rule 4 (no business meaning in core) | SessionStore stores text and source without interpreting them; the notice is C6's generic record; Rpc copies the fields |
| §4 rule 5 (batches) | Not applicable: no Timeline event |
| §4 rule 6 (model context) | No model-visible effect |
| §4 rule 7 (public events and services) | SessionTitle uses the public `store` service; clients use the public notice and list fields |
| §4 rule 8 (infrastructure unchanged) | Timeline, Dispatcher, event formats, assembly, rutis and the Session facade are unchanged. SessionTitle's requirement closes inside SessionStore, its owner, as the overview B.2 supplement records |
| §4 rule 9 (only decision facts in the Timeline) | The title never enters the Timeline; clients see changes through the notice |
| §1 (Capability and App Server boundary) | Inside the existing SessionStore plugin and Rpc; no new plugin |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | Process scope: the title is stored with the Session's rows and outlives every instance |
| ADR 002 D2 (no wiring modules) | The named service only decodes and encodes; one catalog line, as for `sessions` |
| ADR 002 D3 (dependencies follow scope) | SessionStore depends on no optional plugin; `notify` needs no dependency; consumers depend on SessionStore, which is core |
| ADR 002 D4 (configuration) | Not applicable: no configuration |
| ADR 002 D5 (language-neutral contracts) | §4.1 gives the contract as data; Appendix A is the Rust binding |

## 15. Requirements raised to mechanism owners

None. S5b uses C6's `notify`, the existing service catalog and `HostDispatch`, and S5c's delete transaction.

## 16. Acceptance (flow tests)

1. `setTitle` commits and returns the next revision; `title` returns it. Two v3 clients attached over IPC and
   WebSocket each receive exactly one `session.notice` per commit, in commit order, with `data` equal to the list's
   title fields. A rejected change and an unknown Session announce nothing.
2. Concurrent writers: every commit is announced once, in increasing `titleRevision` order.
3. A clear (`null`) commits a revision with no title and announces nulls.
4. `title` and `setTitle` of a Session that was never created, or was deleted: `not_found`.
5. A failed transaction (a disposable store with a failing trigger): `storage`, no notice, state unchanged; the next
   write at the same revision commits.
6. A `setTitle` call whose caller awaits the reply and then drops it, while the write is held before its transaction,
   still commits and announces exactly once (handover, §6).
7. v3 `session.summaries` carries `title`, `titleSource`, `titleRevision`; v2 has no Session listing.
8. A title survives a restart and can be read and written while its Session is not loaded; a fork starts untitled at
   revision 0; a delete removes the row.
9. An empty store is created at v16. Disposable v15 and v13 stores upgrade in place to the exact v16 schema with
   every business row unchanged and report the upgrade once; v16 schemas with a wrong marker, partial upgrades and
   older schemas are refused and preserved.
10. A TypeScript Session part running in Node injects `store`, reads and sets the title, and the attached
    client receives the notice (`node_` test, run by CI's Node step).
11. A `setTitle` request without `title` is rejected; the plugin SDK type check covers the call shapes.
12. Unload: writes admitted before SessionStore starts closing, one in its turn and one waiting for it, commit and
    reply with their revisions while shutdown waits for them; a call after closing starts fails `closed` at once and
    is not in the store after a restart (§6).

Tests: items 1 to 8, 10 and 12 are in [titles.rs](../../../crates/dim-app-server/tests/transport_flow/titles.rs), through
the `store` service as a Session part injects it and the [session-metadata
fixture](../../../runtime/fixtures/session-metadata.ts); items 6 and 12 hold the write with
`storage::testing::hold_title_write` (feature `testing`); item 9 is `a_v15_store_upgrades_in_place_once`,
`a_v13_store_upgrades_in_place_once`, `the_daemon_reports_a_v13_upgrade_on_stderr_once` and the refusal cases in
[storage_flow.rs](../../../crates/dim-app-server/tests/storage_flow.rs); item 11 is the malformed calls in titles.rs and
[plugin-sdk.test.mjs](../../../runtime/plugin-sdk.test.mjs). S5c's delete tests count the `session_metadata` rows.

## 17. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | Table `session_metadata`, native format v16 | A field in `agent_sessions.config`; rows in `settings_documents`; columns on `agent_sessions` | §11 |
| D2 | Revision 0 means no row; a clear keeps the row | Deleting the row on a clear | The counter must never repeat (§3) |
| D3 | The call is the handover; `set_title` admits and starts the write when called | A cancel action until the transaction starts | §6: it moves the uncertainty without removing it, and JavaScript cannot cancel a host call |
| D4 | One process-wide title writer; `notify` inside it, after the commit | Per-Session locks; `notify` under the database lock; relying on the revision alone | Titles are written rarely, so one writer costs nothing. Listener code never runs under the database lock. The revision alone would let clients see notices out of order |
| D5 | Notice `data` is `{title, titleSource, titleRevision}` | A.2's `{sessionId, title, titleSource, revision}` | The channel already names the Session (C6 D4); one field set for list entries and notices lets clients merge without mapping |
| D6 | `titleRevision` in v3 list entries | `title` and `titleSource` only | Without it a client cannot order a list reply against a notice (§8) |
| D7 | List fields for protocol v3 only | All protocol versions | v1 and v2 are frozen and receive no title notices |
| D8 | Service `store` with `title` and `setTitle(id, change)`; the Rust binding uses `title` and `set_title` | A.2's method name `metadata` | `store.title` reads the title; the change carries the decision |
| D9 | Errors `rejected`, `not_found`, `closed`, `storage` as data; malformed calls are bridge errors | A `conflict` code | The change decides the manual rule, so there is no revision conflict |
| D10 | `not_found` for a Session the store does not hold | `session_closed` | Maintainer decision; it is SessionStore's vocabulary, and SessionTitle maps it to `session_not_found` |
| D11 | SessionStore stores text as given | Validating length and characters | SessionTitle owns the title rules and validates them; checking again would duplicate the contract |
| D12 | SessionStore itself provides `store` | A part that never reloads | SessionStore reloads only with SqliteStorage (§12) |
| D13 | The same text written again commits a new revision | Treating it as a no-op | SessionTitle Appendix B expects a duplicate rename to rewrite; a no-op would need its own reply |
| D14 | Admission at the call; unload stops admission, then waits for every admitted write | Admitting a write when it takes its turn, and failing the writes still waiting at unload (the first implementation) | That left a handed-over write without a definite admission: whether it ran depended on whether its task reached the title writer before the disposer did, and a call that arrived during unload was neither refused nor admitted until the write in progress ended. Admitting at the call makes both the handover and the refusal immediate. Titles are written rarely, so waiting for the admitted ones is short. Session files' puts are admitted the same way |

## Appendix A. Rust binding
This section is superseded by [the SessionStore contract](../plugins/SessionStore.md#operations).
