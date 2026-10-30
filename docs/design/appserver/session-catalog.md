# Client Session catalog

Issue: [#556](https://github.com/arcships/dim/issues/556), whose required outcome this page serves: a bounded
`/resume` first page and catalog metadata without decoding full effective-context replacements.
[#557](https://github.com/arcships/dim/issues/557) adds the classification filters this contract reserves a place for.
Overview: [Appendix B.2](../overview.md#b2-app-server-process-plugins), Rpc and SessionStore rows. Scope:
[ADR 002](../../adr/002-plugin-scope.md) process scope, in the existing Agents and Rpc built-ins, with one new Rpc
built-in. Consumers: clients through `session.catalog` and `session.summaries`, first the CLI's `/resume` picker.
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

## 1. Summary

A client lists Sessions with `session.summaries`, and that reply is built from history: for every Session the App
Server reads the batches that start within the first 256 events for a preview and then the last event whole for a
time, one Session after another, and replies one array with no page boundary. An imported Session's last event is a
complete `context/compaction`, so a listing decodes the whole catalog's history, and a client sorts and converts all
of it before it can draw a row.

The catalog read becomes bounded work. SessionStore keeps a projection of what a listing needs — per Session its last
activity time, its first user input as the preview, and its workspace — written by the creating transaction and then
by the transaction that commits each `main` batch. A new built-in, `session.catalog`, pages that projection: 50 rows
by default, an opaque keyset cursor, and `query`, `cwd` and `withPreview` applied in the query before the page
boundary. `session.summaries` keeps its contract and reads the same projection, so neither method reads history.

- **Projection.** SessionStore's, written with its batches and from creation; this page owns only how the catalog is
  read (§4).
- **Method.** `session.catalog {limit?, cursor?, query?, cwd?, withPreview?} -> {sessions, nextCursor?}` (§3.1).
- **Lease-free.** A listing loads no Session, restores none, and starts no model, tool or MCP server (§3.4).
- **Clients.** The picker pages, searches on the server and drops a pending read (§6).

## 2. Scope and non-goals

This page owns the reply contract of `session.catalog`, the catalog read of `session.summaries`, the order, cursor
and page-boundary semantics, and the place where #557's filters apply.

It does not own:

- the projection's table, its indexes, write transaction, delete and fork maintenance, and the upgrade's backfill —
  the [native storage format](native-storage-format.md#556-v23-格式决定) decides those;
- the classification fields, their defaults and their presentation — [#557](https://github.com/arcships/dim/issues/557);
- the picker's interaction and the project/all switch — the client that adds them (§6);
- `session.list`, `session.attach`, and how a client sorts or presents rows.

## 3. Own contract

### 3.1 `session.catalog`

`session.catalog {limit?, cursor?, query?, cwd?, withPreview?} -> {sessions: SessionSummary[], nextCursor?: string}`

| Item | Rule |
| --- | --- |
| Kind | Rpc built-in. Agents answers it from the catalog projection, joined with the `agent_sessions` rows and the title metadata, as it answers the directory read; Rpc validates the envelope |
| Protocol | v3. v1 and v2 reply `unknown_method` ("Method requires protocol v3"), as they do for `session.summaries` |
| Discovery | `hello.features` lists it only while a native store is open, the condition `session.fork` uses, so a client that does not see it keeps using `session.summaries`. Without a store there is no projection to page, and `session.summaries` answers from the loaded Sessions as today (§3.3) |
| `limit` | 1 to 200, default 50. A larger one is rejected, not clamped, as `history`'s `limit` is |
| `cursor` | Opaque. It encodes the last returned row's `(updatedAt, sessionId)` and a digest of the request's `query`, `cwd` and `withPreview`; it pages by keyset, and a client echoes it back unchanged. It is not a Session id |
| Order | `updatedAt` descending, then `sessionId` ascending |
| `query` | Case-insensitive, literal substring match over the four fields the picker searches today — the Session id, the workspace, the model id and the preview — plus the title and the provider id, which its search text never holds. At query time a SQL function the store registers on its connection lowercases the query and each field with `str::to_lowercase`, the picker's fold; the match is `instr(fold(field), fold(query)) > 0` field by field, so it is literal and `%` and `_` match only themselves. It is applied in the catalog query before the page boundary. The model is read from the stored Session config, the value `session.list` reports, so the query decodes no event |
| `cwd` | Exact match on the workspace, applied before the page boundary |
| `withPreview` | Boolean, default `false`. `true` keeps only rows whose preview is not NULL, applied before the page boundary, so a client that hides preview-less rows receives a full page of rows it shows; the projection's partial index over previews keeps that read bounded ([native storage format](native-storage-format.md#556-v23-格式决定)) |
| Reply | `SessionSummary` entries, the shape `session.summaries` returns: the fields of a `session.list` record with `preview` and `updatedAt`. `nextCursor` is absent on the last page |

A `cursor` that does not decode, or whose digest does not match the request's `query`, `cwd` and `withPreview`, is
rejected as `invalid_params` rather than treated as the first page, so a client cannot silently read the catalog from
the start again, or continue one filter set by the key of another. A cursor is not tagged with a store: one taken from
another store's catalog cannot be told apart, and needs no check, because it only positions a keyset in the same
order.

A search with a rare match scans the catalog's rows: the filters read the projection, the stored title and the stored
model, and no filter decodes an event. Registering that function needs rusqlite's `functions` feature and no new
dependency. The fold runs at query time, so no folded value is stored and none has to be kept in step with the title
or the model.

### 3.2 Order, cursor and paging

The order is the projection's activity time, newest first, with the Session id breaking a tie, so a page is a
contiguous run of one total order and the same store always pages the same way. The cursor is the last row's key, not
a position: a Session that becomes active between two pages moves to the first page, and the later pages neither
repeat nor skip any other Session. It also carries the filters it was taken under (§3.1), so a page boundary can be
continued only by the same `query`, `cwd` and `withPreview`.

### 3.3 `session.summaries`

`session.summaries` keeps its parameters, its reply type and its per-client protocol v3 rule. With a native store open
it answers from the same projection instead of reading history, and its contents do not change: the projection
applies the preview rule below unchanged, and its row exists from the Session's creation (§5). A daemon without a
store has no projection and answers from the loaded Sessions as today; it does not list `session.catalog` either
(§3.1). So the method is how a client that does not find `session.catalog` lists, and how clients on a daemon older
than this change list; both stay working because the method's shape does not change.

**The preview keeps today's rule, its batch-complete window included.** The window is the batches that start within
the first 256 `main` events, and it is batch-complete as today's read is: the batch that holds sequence 256 is read
whole, so a user input just past 256 in that batch is inside it. The first `input/message` that carries a user part
among the window's events decides the preview: that part's text blocks joined with newlines, cut to 120 characters and
ending in `…` when the text is longer. The decision is final — a first user message whose text is empty (an image-only
message, for example) leaves `preview` NULL and no later message fills it, and a Session with no such message in the
window keeps NULL. The projection applies the same rule live as in the backfill: a batch may set `preview` only while
the row's preview is still undecided and the deciding message lies in the window, and once decided it never changes.
Backfilled and live rows are therefore column-identical
([native storage format](native-storage-format.md#556-v23-格式决定)). Dropping or narrowing the window would be a
separate, visible change to what `session.summaries` lists.

### 3.4 Lease-free

A catalog read never loads, restores or starts a Session: the projection answers the stored fields, so a listing costs
no model, tool or MCP startup. `running` and `attachedClients` come from the process's in-memory state, as
`session.list` reads them: an unloaded Session reads as not running, and neither field is stored, so reading them
takes no lease and loads nothing.

### 3.5 Catalog filters (#557)

Hierarchy and origin filters are conditions of the same catalog query and apply before the page boundary, so a match
on a later page is never hidden by what the page already fetched. [#557](https://github.com/arcships/dim/issues/557)
defines their fields and defaults. This contract reserves where they apply and requires that a row's position, the
cursor and any count stay consistent across refreshes with them in place.

### 3.6 Cancelling a read

`session.catalog.cancel {requestId} -> {}` cancels an in-flight `session.catalog` or `session.summaries` read on the
same connection. An absent, completed or non-catalog request is a no-op. Protocol v3 advertises this method with
the native catalog. Cancellation never addresses another connection or a mutating request. The cancelled read
returns `cancelled`; a result already completed remains completed.

Closing a connection also cancels its reads. SessionStore checks cancellation before executing a queued read and
interrupts an executing SQLite catalog query. It removes the query's cancellation hook before releasing the shared
connection, so cancellation cannot interrupt a later write. RPC ownership lasts until the SQL operation settles.

## 4. Events and state

This part writes no event and owns no state. The projection is derived state: every column follows from events the
Timeline already holds, and the upgrade rebuilds it once for the whole store from them
([native storage format](native-storage-format.md#556-v23-格式决定)). Nothing here enters the Timeline: the catalog is
display data for a picker, which [design rules §4 rule 9](../../architecture/rust-agent/principles.md) keeps out of it.

## 5. Dependencies

| Dependency | What this part requires |
| --- | --- |
| SessionStore catalog projection | One row per Session with the last activity time, the preview and the workspace, written by the creation transaction with the creation time and no preview, kept by the transaction of every later `main` batch, and absent for a Session that does not exist: the row is 1:1 with `agent_sessions` from creation, so a new Session lists as `session.summaries` lists it today |
| [Title metadata](session-title-metadata.md) | The stored title of a Session, joined for display and for `query`, never read from a Session |
| `hello.features` (App Server) | The method list a client discovers the catalog through: the catalog is listed under the conditions that list `session.summaries`, plus the native-store condition `session.fork` also requires |
| Agents' directory | The protocol-version and agent filtering the catalog applies, as `session.list` applies it |
| Agents' index and the connection hub | The live `running` and `attachedClients` of the listed Sessions, read from the process's state without a lease, as `session.list` reads them |

No Session facade call, no lease and no other plugin service is used.

## 6. Clients

The picker behind `/resume` is the first consumer, and this page states what it requires of a client; the client's
own design owns its interaction.

- **First page.** The first page is the catalog with no `cwd` filter, because `/resume` lists every workspace today.
- **Pages.** The picker asks for the next page as the selection nears the end of the loaded rows, and keeps the
  server's order across pages. It sends `withPreview`, because it hides rows without a preview today, so its first
  page is a full page of rows it shows and the trigger near the end always has rows. Every cold launch creates a
  Session that may never get a message, so without the filter the newest rows would often be ones it hides.
- **Search.** Its filter goes to the server as a debounced `query`, instead of narrowing only the rows it holds, and
  the server matches the fields it searches today — the Session id, the workspace, the model id and the preview —
  plus the title and the provider id, under the same Unicode fold, so a match on a page it has not fetched still
  appears.
- **Cancellation.** Closing, refreshing or replacing a search sends `session.catalog.cancel` for the pending read
  when advertised, then drops its local request identity. A command entered afterwards does not wait for the obsolete
  query. An older daemon without cancellation discovery retains its existing behavior.
- **Fallback.** A daemon that does not advertise the method is listed through `session.summaries`.

```text
/resume -> [saved Sessions | search...]
             Esc       -> cancel read -> composer
             Ctrl+R    -> cancel read -> refreshed first page
             new query -> cancel read -> matching first page
```

The project/all switch is a separate client decision: `cwd` exists in this contract for it and applies before the
page boundary, and the picker's first page does not use it. The gap is recorded in
[Clients](../clients.md#32-session-management).

## 7. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| C1 | The cursor is opaque and pages by keyset on `(updatedAt, sessionId)` | Offset paging with `offset` or `skip` | An insert shifts every later row, so an offset page repeats or skips Sessions the client already showed |
| C2 | A new built-in, `session.catalog` | Adding `limit` and `cursor` to `session.summaries` in place | Its reply is an array; changing it breaks every protocol v3 client of that method, and an array has no page boundary to keep |
| C3 | The preview and the activity time are read from the projection | Computing them per request from history | That is the cost this design removes: it reads every Session's history, and an imported Session's last event is a complete compaction replacement |
| C4 | `query` and `cwd` are applied in the catalog query | Filtering the rows the client already loaded | It hides matches on pages the client has not fetched |
| C5 | `session.summaries` keeps its contract and reads the same projection | Leaving it on the history read; removing it | It is how a client lists without the new method, and once the projection answers both, keeping its contract costs nothing |
| C6 | Neither method takes a Session lease | One lease per listed Session | A lease restores and pins every listed Session; the projection, and the process's live state for `running` and `attachedClients`, answer without one |
| C7 | The preview is today's rule over its batch-complete window, applied identically by a live batch and by the backfill | An unbounded live rule, writing the preview with the batch that carries the first user input; a strict cutoff after sequence 256 | It changes what `session.summaries` lists for a Session whose first user message follows the window, or whose first user input is just past sequence 256 in the batch that holds 256, and leaves backfilled and live rows different. Dropping or narrowing the window is a separate, visible change |
| C8 | `withPreview` drops preview-less rows in the catalog query, over the projection's partial index | A client that fetches pages until it holds enough rows it shows | A cold launch creates a Session that may never get a message, so the newest rows are often preview-less and that loop reads an unbounded run of them; the partial index keeps the server read bounded |
| C9 | `query` folds both sides with Unicode `str::to_lowercase` at query time, as the picker does | ASCII-only matching through SQLite's `LIKE`, which folds ASCII alone | It would silently miss accented and non-Latin case pairs the picker finds today |

## 8. Changes to other documents

- [Native storage format](native-storage-format.md#556-v23-格式决定): the catalog table, its two indexes, the
  same-transaction write from creation on, delete and fork maintenance, and the upgrade's backfill and its one-time
  cost.
- [Session deletion and fork](session-deletion-and-fork.md#32-sequence): its delete and fork row lists name
  `session_catalog` beside the other rows they remove and write.
- [Documentation index](../../INDEX.md) lists this page.
- [Clients](../clients.md#32-session-management) §5 names `session.catalog` where the live Session list row names
  `session.summaries`, and the CLI documents `/resume`'s paging, server search and cancellation. Both describe
  behaviour that exists only once the client ships them, so the client's implementation makes those two edits.

## 9. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (a command changes another owner's state) | Nothing is written. The catalog reads rows SessionStore owns, through Agents |
| §4 rule 2 (handover) | None: a read hands nothing to another module |
| §4 rule 3 (before callbacks) | None |
| §4 rule 4 (no business meaning in core) | No core change; the projection is SessionStore's business row |
| §4 rule 5 (batch) | The row is written in SessionStore's existing batch transaction; the catalog adds no batch and no event |
| §4 rule 6 (model context) | Unchanged: no event, so no context path |
| §4 rule 7 (public events and services) | Agents answers from SessionStore's stored rows, as `session.list` does; Rpc reaches neither the store nor another plugin's state directly |
| §4 rule 8 (infrastructure unchanged) | The Timeline, Dispatcher, event envelope and format, assembly and the Session facade are unchanged. The store gains a derived table and no event, by the storage page's own format decision |
| §4 rule 9 (only facts later decisions use enter the Timeline) | The catalog is picker display data, so it stays out of the Timeline; no event is added |
| ADR 002 D2 (no wiring module) | No new plugin: Rpc's built-in and Agents' existing directory answer it |
| ADR 002 D5 (language-neutral contracts) | §3 is the contract as data; the Rust spelling is the implementation's |

## 10. Acceptance (flow tests)

- A first page of 50 by default, an explicit `limit` within 1 to 200, and a last page without `nextCursor`.
- Paging by the returned cursor returns every remaining Session exactly once in the stated order, including when a
  Session is updated between two pages.
- `query` finds a Session by each of its six fields — the title, the preview, the Session id, the workspace, the
  model id and the provider id — without regard to case, including a case pair outside ASCII, and finds a Session
  that is only on a later page; `cwd` matches the workspace exactly; `withPreview` leaves out a Session with no
  preview; all apply before the page boundary.
- A `%` or `_` in `query` matches only that character.
- A page with `withPreview` returns `limit` rows even when many preview-less Sessions precede them in the order.
- A cursor returned for one combination of `query`, `cwd` and `withPreview` is rejected as `invalid_params` when
  replayed with another.
- A Session created and never written to is listed from its creation with `updatedAt` equal to its creation time and
  no preview, as `session.summaries` lists it today.
- The preview keeps its batch-complete window and its finality: a user input just past sequence 256 in the batch that
  holds 256 lists as the preview, both live and after a backfill; a Session whose first user message is in a batch
  that starts after sequence 256 lists with no preview; and one whose first user message is image-only lists with no
  preview that a later message fills.
- A daemon with no native store does not list `session.catalog`, and its `session.summaries` answers from the loaded
  Sessions as today.
- A Session whose `main` ends in a full `context/compaction` is listed with its stored preview without decoding that
  event.
- A listing taken while the Session is idle unloaded replies the same stored fields as while it is loaded, reads it
  as not running, and starts no model, tool or MCP server.
- `session.summaries` keeps its reply shape and contents for protocol v3.
