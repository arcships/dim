# SessionStore title metadata

Status: Implemented in roadmap item S5b ([#60](https://github.com/arcships/dim/issues/60),
[#96](https://github.com/arcships/dim/pull/96)), after C6 ([#49](https://github.com/arcships/dim/issues/49),
[#85](https://github.com/arcships/dim/pull/85)); the implementation follows this design. Review refined when a write
is admitted and how unload settles it (§6, D14). Requirement: [SessionTitle](../plugins/SessionTitle.md) §2, §4 and
[A.2](../plugins/SessionTitle.md#a2-depended-on-shapes-owners-define-them). Overview:
[Appendix B.2](../overview.md#b2-app-server-process-plugins), SessionStore and Rpc rows. Scope:
[ADR 002](../../adr/002-plugin-scope.md) process scope, inside the existing SessionStore plugin, with three Rpc list
fields. Consumer: SessionTitle.

## 1. Summary

SessionStore keeps one title per Session with a per-Session revision. The title changes only through `setTitle`, a
revision-checked write. After each committed change SessionStore announces the new title on the Session's broadcast
channel ([C6](session-notices.md)), and Rpc shows the title in protocol v3 Session lists.

- **Data.** A revision counter (0 means never written) and either no title or a title `{text, source}` with source
  `auto` or `manual`. Clearing keeps the counter, so a revision never repeats.
- **Write.** `setTitle {sessionId, expectedRevision, title | null}` commits revision `expectedRevision + 1` or fails
  `conflict`, `not_found` or `storage`. The call is the handover: SessionStore admits the write when called, unless it
  is closing, and settles it itself; dropping the call only loses the reply.
- **Notice.** After each commit, `{source: "SessionStore", kind: "title", data: {title, titleSource,
  titleRevision}}`, in commit order.
- **Lists.** Protocol v3 `session.summaries` entries carry `title`, `titleSource` and
  `titleRevision`.
- **Binding.** The process service `sessionMetadata` (`read`, `setTitle`), provided by SessionStore, reaches
  TypeScript through the existing service catalog, like `sessions` and `settings`.
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
   inject 'sessionMetadata'               sessionMetadata (HostDispatch)
     read -------------------------->       read     -> metadata()   ------------------+
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
| SessionStore ([timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs), [metadata.rs](../../../crates/dim-app-server/src/storage/metadata.rs)) | Process | `metadata` and `set_title`, the title writer, the `sessionMetadata` service; delete and creation rollback remove the title row; the catalog query reads it |
| SqliteStorage ([sqlite.rs](../../../crates/dim-app-server/src/storage/sqlite.rs)) | Process | v16 schema and its upgrade step |
| Rpc ([protocol.rs](../../../crates/dim-app-server/src/protocol.rs), [connection.rs](../../../crates/dim-app-server/src/connection.rs)) | Process | The v3 list fields |
| Service catalog ([lib.rs](../../../crates/dim-app-server/src/lib.rs)) | Process | `register_shared("sessionMetadata")`, one line, as S2 did for `sessions` |
| Session broadcast, Rpc notice forwarding | Session, process | Unchanged (C6) |

## 3. Data and revisions

| Item | Meaning |
| --- | --- |
| `revision` | Per-Session counter of committed `setTitle` writes. 0 means never written. Each committed write sets it to `expectedRevision + 1`: a new title, the same text again, a source change or a clear |
| `title` | `{text, source}` or none |
| `source` | `auto` (SessionTitle's generation) or `manual` (`session.rename`). SessionStore stores the source it is given and attaches no rule to it. "A manual title always wins" is SessionTitle's rule, enforced through the revision check ([SessionTitle §4](../plugins/SessionTitle.md#4-timing-and-conflicts)) |

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

`sessionMetadata`, process scope, control node only, no instance key, provided by SessionStore.

| Method | Mode | Request | Reply value | Errors |
| --- | --- | --- | --- | --- |
| `read` | async | `{sessionId}` | `{revision, title: {text, source} \| null}` | `not_found`, `storage` |
| `setTitle` | async | `{sessionId, expectedRevision, title: {text, source: "auto" \| "manual"} \| null}` | `{revision}`: the committed revision | `conflict`, `not_found`, `storage` |

- Replies use one envelope, `{ok: true, value}` or `{ok: false, error: {code, message}}`, as `settings` does.
- Neither method loads the Session. `read` reads committed state and does not wait for the title writer (§5).
- `title` is required in `setTitle`; `null` clears. A request without `title` is rejected rather than read as a
  clear, so a caller's omission never erases a user's title.
- A malformed call (unknown method, not exactly one request object, an unknown field, a wrong type or a missing
  field) is a bridge error, as for `settings`: it is a caller's programming error, not an outcome of the write.

| Code | When | State after |
| --- | --- | --- |
| `not_found` | The store holds no record of the Session: never created, deleted, or rolled back. A stored Session that is closed or not loaded is found | Unchanged |
| `conflict` | `expectedRevision` is not the current revision | Unchanged, no notice. Read again |
| `storage` | The transaction failed, or the call arrived after SessionStore started closing (§6) | Unchanged, no notice |

### 4.2 Rust

Rust consumers inject `SessionMetadataService`, the typed binding of `sessionMetadata`, and call
`read` and `set_title`. The binding is the existing dispatcher, published at both its typed key
and `host_key("sessionMetadata")`; both routes delegate directly to the same SessionStore owner.
The existing `SessionMetadata` reply type is unchanged.

`set_title` is a regular function returning the owner's wait future. Calling it admits the write
immediately, before that future is polled; dropping the future loses only the reply. The binding
adds no asynchronous wrapper before handover, cancellation point, read, or second title writer.

## 5. setTitle sequence

```text
 caller                  SessionStore                                     SQLite             Session channel
 ------                  -----------------------------------------------  -----------------  ---------------
 setTitle(S, r, T) ----> SessionStore closing? -> storage at once, nothing written
                         admit: start the write task   <== handover (the call)
                           |  (SessionStore owns the task; dropping the
                           |   call from here on only loses the reply)
                           |  wait for its turn at the title writer
                           |  (one write at a time)
                           +-- BEGIN IMMEDIATE ---------------------------> revision of S
                           |                                                 no Session: not_found, rollback
                           |                                                 revision != r: conflict, rollback
                           |                                                 write r+1 and T
                           +-- COMMIT ------------------------------------> committed, or storage
                           |  committed only:
                           |    notify(S, {SessionStore, title,
                           |      {title, titleSource, titleRevision: r+1}}) -------------------> Notice
                           |  end its turn
 <---- {revision: r+1} ----+
```

`notify` runs after the commit and before the next write takes its turn, so the notices of one Session reach its
channel in commit order. It never runs under the SQLite connection lock: listener code does not run under the database
lock (the rule in [timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs)).

## 6. Handover, cancellation and settlement

This settles the boundary that SessionTitle §0 left to S5b.

| Point | Rule |
| --- | --- |
| Before the call | The caller's own work. Cancelling it before the call writes nothing |
| The call | The handover is SessionStore receiving the call: before it returns, `setTitle` admits the write and starts it as SessionStore's own task, or, once SessionStore is closing, fails `storage` without writing. In Rust `set_title` is a plain function that does this and returns a future of the reply. That future only awaits the task, so dropping it detaches the reply, never the write. The named service calls it inside `invoke`, so a JavaScript call is handed over once the host receives it |
| After the call | SessionStore settles the write itself: it commits and announces, or fails and writes nothing. Dropping the call, unloading the calling row or losing the reply neither cancels nor undoes it. `setTitle` has no cancel action |
| Lost reply | The caller reads with `read`. It does not repeat the write blindly: a repeat at the old revision fails `conflict`, and one at the new revision would be a second change ([SessionTitle Appendix C](../plugins/SessionTitle.md#appendix-c-errors-and-recovery)) |
| SessionStore unload | Its disposer first stops admission: from then on a call fails `storage` at once and writes nothing. It then waits until every admitted write, the one in its turn and those waiting for theirs, has committed and announced or failed. Only then does it close Session files and the Timeline writer, and SqliteStorage, which unloads after SessionStore, closes the connection. The notices follow C6 delivery: once the Session is no longer loaded, nobody listens and they are dropped. Nothing is retried after a reload |

Rejected: a cancel action that works until the transaction starts. It would only move the uncertainty, since a caller
that drops the call still has to read to learn the outcome; and a JavaScript caller cannot cancel a host call in rutis
([sessions §2](sessions-service.md#2-spike-findings), finding 5).

## 7. The title notice

| Field | Value |
| --- | --- |
| `source` | `"SessionStore"` |
| `kind` | `"title"` |
| `data` | `{title: string \| null, titleSource: "auto" \| "manual" \| null, titleRevision}` |

- **Once per commit.** Emitted after each committed write with SessionStore's own context through
  `session_broadcast::notify`. A conflict, a missing Session or a failed write emits nothing.
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
| `conflict` | — | SessionTitle reads again and retries ([§3.2](../plugins/SessionTitle.md#32-manual-rename)) |

## 9. Lifecycle

| Event | Title |
| --- | --- |
| Session created | None, revision 0; no row is written |
| Load, unload, close, restart | Unchanged: the title is stored with the Session's rows, not with its instance. `read` and `setTitle` work on a Session that is not loaded; their notices are dropped, as nobody listens |
| `session.delete` (S5c) | Removed in S5c's single delete transaction, before the Session record. Afterwards `read` and `setTitle` fail `not_found` |
| Creation rollback | The same transaction as a delete; it removes a row that a racing write committed |
| `session.fork` (S5c) | Not copied ([SessionTitle A7](../plugins/SessionTitle.md#appendix-e-assumptions)): the fork starts untitled at revision 0 |
| SessionStore or SqliteStorage reload | Settles as in §6. Consumers of `sessionMetadata` reload with SessionStore ([ADR 002](../../adr/002-plugin-scope.md) D3), which SessionTitle §1 accepts. SessionStore reloads only with SqliteStorage, which has no dependencies |

## 10. Timing and conflicts

| Scenario | Outcome |
| --- | --- |
| Two writes with the same expected revision | The first to commit wins; the other fails `conflict` and announces nothing |
| Writes to different Sessions | Run one at a time by the one title writer; each commits in its own transaction |
| `setTitle` and `session.delete` | The store's write transactions order them: a commit first is removed by the delete; a delete first makes the write `not_found` |
| `setTitle` and `session.fork` | Independent: the fork copies no title |
| `setTitle` and Timeline appends | Independent rows; each uses its own transaction on the one store connection |
| Notice and list read on one connection | §8: the higher `titleRevision` wins |
| `setTitle` with no client attached | Committed; the notice is dropped; the next list shows the title |
| The same text written again | Commits a new revision and announces it ([SessionTitle Appendix B](../plugins/SessionTitle.md#appendix-b-full-conflict-table): a duplicate rename rewrites) |

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
- **Index.** The primary key serves `read`, `setTitle`, the list join and the delete. No other index.

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
| Identity | `dyn HostDispatch` at `host_key("sessionMetadata")`, provided by SessionStore; `register_shared("sessionMetadata")` in App Server's service catalog, beside `sessions` and `settings` |
| Use | A C4 Session part declares `inject: ['sessionMetadata']` and calls `ctx.use('sessionMetadata')`, the path `settings` takes from a Session part ([settings-owner fixture](../../../runtime/fixtures/settings-owner.ts)). Types are in [npm/plugin.d.ts](../../../npm/plugin.d.ts) |
| Modes | `read` and `setTitle` are both `async` |
| Writer | The service carries no caller identity, as for `settings` (AGENTS.md rule 11). SessionTitle is the title's one writer by design ([SessionTitle §2](../plugins/SessionTitle.md#2-data-and-state)); other packages only read |
| Reload | A consuming row reloads with SessionStore. SessionStore reloads only with SqliteStorage, so no directory part that never reloads is needed, unlike `sessions`, whose index reloads with Settings ([sessions D1](sessions-service.md#3-decisions)) |
| Missing provider | A daemon without a native store has no SessionStore, so a row that injects `sessionMetadata` stays Pending under the existing rules; native Sessions continue |

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
| [TypeScript plugin authoring](../../typescript-plugin-authoring.md), [npm/plugin.d.ts](../../../npm/plugin.d.ts) | The `sessionMetadata` service and its types |

## 14. Rule check

| Rule | How this design complies |
| --- | --- |
| [Principles](../../architecture/rust-agent/principles.md) §4 rule 1 (commands for others' state) | The title is SessionStore's process state, outside any Timeline. Others change it only through SessionStore's typed `setTitle`, and SessionStore decides through its revision check |
| §4 rule 2 (handover) | The handover is explicit: the call (§6). Before it the caller can cancel; after it only SessionStore settles the write |
| §4 rule 3 (before callbacks) | Not applicable: no before callback |
| §4 rule 4 (no business meaning in core) | SessionStore stores text and source without interpreting them; the notice is C6's generic record; Rpc copies the fields |
| §4 rule 5 (batches) | Not applicable: no Timeline event |
| §4 rule 6 (model context) | No model-visible effect |
| §4 rule 7 (public events and services) | SessionTitle uses the public `sessionMetadata` service; clients use the public notice and list fields |
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

1. `setTitle` commits and returns the next revision; `read` returns it. Two v3 clients attached over IPC and
   WebSocket each receive exactly one `session.notice` per commit, in commit order, with `data` equal to the list's
   title fields. A conflict and an unknown Session announce nothing.
2. Concurrent writers: every commit is announced once, in increasing `titleRevision` order.
3. A clear (`null`) commits a revision with no title and announces nulls.
4. `read` and `setTitle` of a Session that was never created, or was deleted: `not_found`.
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
10. A TypeScript Session part running in Node injects `sessionMetadata`, reads and sets the title, and the attached
    client receives the notice (`node_` test, run by CI's Node step).
11. A `setTitle` request without `title` is rejected; the plugin SDK type check covers the call shapes.
12. Unload: writes admitted before SessionStore starts closing, one in its turn and one waiting for it, commit and
    reply with their revisions while shutdown waits for them; a call after closing starts fails `storage` at once and
    is not in the store after a restart (§6).

Tests: items 1 to 8, 10 and 12 are in [titles.rs](../../../crates/dim-app-server/tests/transport_flow/titles.rs), through
the `sessionMetadata` service as a Session part injects it and the [session-metadata
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
| D8 | Service `sessionMetadata` with `read` and `setTitle`; the Rust binding uses `read` and `set_title` | A.2's method name `metadata` | `sessionMetadata.metadata` repeats itself; `read` matches `settings.read` |
| D9 | Errors `conflict`, `not_found`, `storage` as data; malformed calls are bridge errors | A `rejected` code as in `sessions` | The three codes are the write's outcomes; a malformed call is a programming error, as in `settings` |
| D10 | `not_found` for a Session the store does not hold | `session_closed` | Maintainer decision; it is SessionStore's vocabulary, and SessionTitle maps it to `session_not_found` |
| D11 | SessionStore stores text as given | Validating length and characters | SessionTitle owns the title rules and validates them; checking again would duplicate the contract |
| D12 | SessionStore itself provides `sessionMetadata` | A part that never reloads | SessionStore reloads only with SqliteStorage (§12) |
| D13 | The same text written again commits a new revision | Treating it as a no-op | SessionTitle Appendix B expects a duplicate rename to rewrite; a no-op would need its own reply |
| D14 | Admission at the call; unload stops admission, then waits for every admitted write | Admitting a write when it takes its turn, and failing the writes still waiting at unload (the first implementation) | That left a handed-over write without a definite admission: whether it ran depended on whether its task reached the title writer before the disposer did, and a call that arrived during unload was neither refused nor admitted until the write in progress ended. Admitting at the call makes both the handover and the refusal immediate. Titles are written rarely, so waiting for the admitted ones is short. Session files' puts are admitted the same way |

## Appendix A. Rust binding

In `dim_app_server::storage`:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource { Auto, Manual }
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title { pub text: String, pub source: TitleSource }
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SessionMetadata { pub revision: u64, pub title: Option<Title> }
/// The title fields of a v3 Session list entry and the title notice's data.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleFields { pub title: Option<String>, pub title_source: Option<TitleSource>, pub title_revision: u64 }
pub enum MetadataError { Conflict, NotFound, Storage(String) }

impl SessionMetadataService {
    pub async fn read(&self, session_id: &str) -> Result<SessionMetadata, MetadataError>;
    /// Admits and starts the write when called, or fails `Storage` once
    /// SessionStore is closing: the call is the handover. Dropping the
    /// returned future loses only the reply.
    pub fn set_title(&self, session_id: &str, expected_revision: u64, title: Option<Title>)
        -> impl Future<Output = Result<u64, MetadataError>> + Send + 'static;
}
```
