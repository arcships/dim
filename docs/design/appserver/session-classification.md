# Session classification: creation origin and catalog visibility

Issue: [#557](https://github.com/arcships/dim/issues/557). The catalog read this classification feeds:
[Client Session catalog](session-catalog.md) (#556). Storage step:
[native storage format](native-storage-format.md#557-v30-格式决定). Neighbors that keep their own answers:
[#795](https://github.com/arcships/dim/issues/795) (which model the catalog reports), [#742](https://github.com/arcships/dim/issues/742)
(a browser-local archive list, not a catalog class). Scope: process — SessionStore's stored Session record and the
App Server reads over it. No Session or branch part. Rules: [design rules §4](../architecture/rust-agent/principles.md),
[ADR 002](../../adr/002-plugin-scope.md) D1–D5.

## 1. Summary

A picker cannot tell a user's own conversation from a Session an editor created over ACP or from a Session Dim created
for its own work. After a large legacy import the last kind is most of the list, and most of those rows carry the same
delegation prompt as their preview, so `/resume` reads as one repeated conversation.

The Session record gains one creation-only field, `origin`, with four values: `client`, `acp`, `internal` and
`unknown`. The entry that creates a Session writes it in the creation transaction; nothing else writes it except an
import that replaces `unknown` with what the source recorded. The parent of a child Session is the leader link the
store already keeps. Four reads report both — `session.list`, `session.attach`, `session.summaries`,
`session.catalog` — and only `session.catalog` can drop whole classes: `withInternal` and `withAcpSessions`, both off by
default, applied before the page boundary. A picker that turns neither on lists conversations, which is what the legacy
picker did.

- **One field, one writer.** Creation fixed; a later client attachment never relabels a Session.
- **No guessing.** `unknown` is a value, not an absence: a row imported before its provenance is mapped stays visible
  and explicit rather than being classified from its id or its message text.
- **No new read.** A list already joins `agent_sessions` for every row; the classes are two columns of that row.

## 2. Scope and non-goals

This page owns the type of the origin and its meaning, which creation entry writes which value, the one later write an
import may make, the two protocol fields that report it, the `session.create {origin}` parameter, the two discovery
markers, and what a client must do with them.

It does not own:

- the catalog query, its order, cursor, `query`, `cwd` and `withPreview`, and where the two visibility filters are
  applied: [Client Session catalog](session-catalog.md#31-sessioncatalog);
- the storage column, its upgrade step and its backfill: [native storage format](native-storage-format.md);
- which legacy metadata becomes which origin, and how old parent ids become new ones: [Migration](../plugins/Migration.md);
- Collaboration's member role, its tasks and its lifetime: [Collaboration](../plugins/Collaboration.md);
- the picker's interaction and where its preference is saved: [CLI commands](../cli-commands.md).

## 3. Own contract

### 3.1 The origin

| Value | Meaning |
| --- | --- |
| `client` | A client asked for a conversation Session through `session.create` and declared no more specific origin |
| `acp` | An editor created it over ACP (`session/new`), which sends `session.create {origin: "acp"}` |
| `internal` | Dim created it for its own work: a Collaboration member, whose leader is its parent, or a Session a process plugin launched through `sessions.launch` |
| `unknown` | The store does not record it: the Session predates the field, or an import did not map the source's provenance |

`internal` is the class [#557](https://github.com/arcships/dim/issues/557) calls *child Sessions*. The value is not named
`child` because a child needs a parent and internal work need not have one: a Collaboration member has a leader and
reports `parentSessionId`, while a Session a process plugin launched is internal work with no parent. The two facts are
separate fields, so a client that wants the issue's child reads `origin: "internal"` together with a present
`parentSessionId`; a client that hides child Sessions sends neither the internal class nor the ACP class.

The origin is recorded once, in the creation transaction, and is fixed for the Session's life. It is not derived from
the connection's `clientKind` (a connection is not durable), from `interactive` (a runtime property a client may set),
from `collaboration_role` (runtime semantics; the paragraph below), from the Session id or from message text. A
classification read from prompt text would relabel a Session when a title changes and would still be wrong for the
image-only and imported cases.

`internal` says who created a Session, not what it may do. It sets no collaboration role, mounts no tool, creates no Team
and starts no work: an imported member is an inert conversation. That is why the catalog cannot reuse
`collaboration_role` as the label, and why an import must not set that role to fake one.

The only later write: an import may replace `unknown` with a value the source itself recorded, once. Nothing writes a
stored origin back to `unknown`, and no other writer changes it.

**Parent.** `parentSessionId` is the leader Session's id when the store records one
(`agent_sessions.leader_session_id`, [#593](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)),
absent otherwise. The link names a Session, not its state: the parent may be closed and so not listed. A Session whose
parent is not in this store lists without the link, because the stored value is a foreign key and no row references a
Session the store does not hold.

### 3.2 Which creation entry writes which value

| Entry | origin | parent |
| --- | --- | --- |
| `session.create` without `origin` | `client` | none |
| `session.create {origin: "acp"}`, from AcpGateway's `session/new` | `acp` | none |
| `session.fork` | `client`, never copied from the source | none |
| Collaboration member creation | `internal` | the leader |
| `sessions.launch` | `internal` | none |
| Native store v30 upgrade | `internal` where a leader link is stored, else `unknown` | as stored |
| Migration | `unknown`, or a mapped legacy value | the translated parent id when the source recorded one and that Session was imported; a parent that was deferred, failed or is absent from the store leaves the link absent, so the child still lists as `internal` without one |

A fork is a new Session of the client that forked it, for the reason the legacy catalog gave: its source's provenance
does not carry over, and an ACP-origin Session's fork is not itself an ACP Session.

### 3.3 Reported fields

`SessionInfo` gains two protocol v3 fields; every reply that carries a `SessionInfo` reports them and none derives them:

| Field | Value |
| --- | --- |
| `origin` | The stored origin. Absent for protocol v1 and v2, and for a daemon with no native store, which stores no creation record |
| `parentSessionId` | The parent's id, present only when the store records one |

| Reply | Effect |
| --- | --- |
| `session.list` | Reports both; applies no visibility filter |
| `session.attach` | Reports both, from one indexed row read of `agent_sessions` for protocol v3 (the title read it already does is on another table); applies no visibility filter |
| `session.summaries` | Reports both; applies no visibility filter, so its contents do not change |
| `session.catalog` | Reports both; applies the two filters of §3.5 |

A client that opens a known Session by id therefore reads the class and the parent from the attach reply, without a
catalog read and without the Session being listed.

### 3.4 `session.create {origin}`

`session.create` accepts `origin?: "client" | "acp"`; absent means `client`. `internal` and `unknown` are refused as
`invalid_params`: a client does not create Dim's internal work and cannot declare provenance it does not have. The
parameter is sent only when the daemon advertises `session.create.origin`, because an older daemon rejects unknown
create parameters; a client that sends it blind fails its whole creation.

`session.fork` takes no such parameter: a fork is the forking client's Session (§3.2).

### 3.5 Catalog visibility

The two filters are query parameters of [`session.catalog`](session-catalog.md#31-sessioncatalog), which owns their
names, defaults, cursor digest and page boundary. This page defines what they select:

| Filter | Default | Effect when `false` |
| --- | --- | --- |
| `withAcpSessions` | `false` | Rows whose origin is `acp` are not listed |
| `withInternal` | `false` | Rows whose origin is `internal` are not listed |

`client` and `unknown` are always listed. An imported conversation whose provenance was not mapped must not disappear
from the picker: `unknown` is what the store knows, and hiding it would delete a user's history from the only list they
have.

`query`, `cwd`, `withPreview` and the order are unchanged and never read the origin; the two filters are additional
conditions of the same query, so they compose with a search and apply before the same page boundary.

A client that only wants to page every Session sends both filters `true`. A daemon that does not advertise
`session.catalog.origins` lists every Session as before, which is what a client of that daemon shows.

### 3.6 Cost of a listing

A filtered page adds no read and decodes nothing:

- The catalog query already joins `agent_sessions` for each row it returns. The origin and the parent are two columns of
  that row: no config JSON decode, no history read, no Session load, no per-Session store call.
- A filter prunes rows while the query walks its activity index, and the page stops after `limit + 1` matches. So a page
  costs one page plus the rows it skips: a store that is mostly internal pays one extra row visit per hidden row, and a
  row visit is a column read.
- No new index. The two switches select two values of one column in four combinations; a partial index serves one
  combination and is maintained by every commit that moves `updated_at_ms`, the cost [#596](https://github.com/arcships/dim/issues/596)
  removed an index for. If the implementation's measurement shows skipped rows matter on a real imported store, that is
  a measurement and a decision of its own, not an index added silently.

### 3.7 Errors

| Case | Reply |
| --- | --- |
| `session.create {origin}` outside `client` and `acp` | `invalid_params` |
| `withInternal` or `withAcpSessions` not a boolean | `invalid_params`, from parameter decoding |
| A catalog cursor taken under one filter set and replayed under another | `invalid_params` ([catalog](session-catalog.md#32-order-cursor-and-paging)) |
| A stored origin this build does not know | Cannot reach a reply: the column's `CHECK` and the store's exact-schema gate make it a refused store, not a row read with a fallback |

### 3.8 Client requirements

- **Default request.** The picker sends `withInternal: false` (or omits it) and `withAcpSessions` from its preference,
  and sends either only when `session.catalog.origins` is advertised.
- **Preference.** "Show ACP sessions" is a persisted client preference, off by default, saved with the client's other
  interface switches (in the CLI, `config.json` through `/settings`). It is not a daemon setting and not stored on a
  Session; a client that migrates its own legacy settings carries its own value over, and the default otherwise.
- **Labels.** A listed row is labelled by origin: a conversation, an ACP Session, an internal Session. An internal row a
  client shows carries its parent link when `parentSessionId` is present. The label is separate from the row's text,
  which is the stored title when there is one, else the preview, else the Session id; a repeated delegation prompt
  therefore no longer stands in for a title, and a classification never replaces the title.
- **Fallback.** A daemon without the marker keeps today's request and today's labels.

```text
+ Sessions ------------------ Show ACP: Off +
| > Search conversations                    |
| [Chat] Update cache              2h ago   |
| [Chat] Investigate startup        3h ago   |
+-------------------------------------------+

+ Sessions ------------------- Show ACP: On +
| [Chat] Update cache              2h ago   |
| [ACP ] Editor conversation       1h ago   |
+-------------------------------------------+

Show ACP: Off / On        /settings switch, saved to the client's config
[Chat] [ACP ] [Agent]     row labels, by origin
Parent: Update cache      shown where a client lists internal rows
```

### 3.9 Compatibility

| Subject | Effect |
| --- | --- |
| Protocol version | No bump. Both filters and both fields are optional additions; a client that sends neither gets the top-level view, which is the change this issue asks for |
| Protocol v1 and v2 | `session.list` items carry neither field, as they carry no title or `agentId`; the catalog is not served at all |
| Older daemons | A client sends `origin` and the two filters only when the marker is advertised, so its request stays valid against a daemon that rejects unknown parameters |
| Older clients | A v3 client that predates the filters lists conversations from a new daemon, and labels nothing; it sees fewer rows than before and no wrong ones |
| `session.summaries` | Lists every Session as before, now with the two fields on each item; ACP's `session/list` is unchanged |
| Existing stores | v30 is one-way; a v29 daemon refuses the store at startup as it refuses any later format. The upgrade hides exactly the rows whose leader link it records, and leaves every other row visible as `unknown` |
| Existing Sessions | No Session is reclassified later by a read, a title change, a model change or an attachment |

## 4. Events and state

This part writes no event, owns no Session or branch state and adds nothing to the model context.

| State | Owner | Written | Rebuilt |
| --- | --- | --- | --- |
| `agent_sessions.origin` | SessionStore's stored Session record | The creation transaction, once; later only an import replacing `unknown` | Not from the Timeline: it is creation input, so the projection property of [the catalog](session-catalog.md#4-events-and-state) does not extend to it. The v30 step backfills what the store already records (the leader link) and leaves the rest `unknown` |
| `agent_sessions.leader_session_id` | same | Collaboration member creation ([#593](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)) | As stored |

The class is display data for pickers. [Design rules §4 rule 9](../architecture/rust-agent/principles.md) keeps it out of
the Timeline and out of the model's context; a Session's origin also never reaches model requests.

## 5. Dependencies

| Dependency | What this part requires |
| --- | --- |
| SessionStore's stored Session record | The `origin` column, written by the creation transaction, and the stored leader link; read by the directory, the catalog and the attach record |
| Agents' creation entries | Each passes the value it owns: `session.create` passes `client` or the declared `acp`, Collaboration passes `internal` with the leader, `sessions.launch` passes `internal` |
| Agents' `session_record` (attach) | One indexed row read of the Session's creation record for protocol v3 |
| [Client Session catalog](session-catalog.md) | Applies `withInternal` and `withAcpSessions` in its query before the page boundary and keeps both out of `session.summaries` |
| Rpc discovery (`hello.features`) | Advertises `session.create.origin` beside `session.create`, and `session.catalog.origins` beside `session.catalog` |
| [AcpGateway](../plugins/AcpGateway.md) | Declares `acp` on `session/new` |
| [Migration](../plugins/Migration.md) | Writes `unknown`, and later the mapped value |

## 6. Cross-module requirements

Format of [design rules §4 rule 8](../architecture/rust-agent/principles.md).

| Problem | Why existing mechanisms fail | Needed capability | Ordering | Owner |
| --- | --- | --- | --- | --- |
| Imported Sessions keep no child or ACP classification, and old parent ids have no native target | `SessionConfig` has no origin; the source's settings and `session_relations` are archived but not mapped; `collaboration_role` carries runtime meaning and must not be set as a label | The `origin` column, its one-write rule, and an import that may replace `unknown` once; Migration writes native rows directly as it does for config and `cwd`, so no App Server method is added | After this contract and format v30 | Migration ([#557](https://github.com/arcships/dim/issues/557)) |
| A.5 says ACP Session display is unsupported for want of a durable client-origin field | The field now exists | `session/new` sends `origin`, and A.5 describes the field | With the implementation | AcpGateway |
| `/resume` has no class labels and no ACP switch; `/settings` lists ACP display as unsupported | The reply carries no class | Filters, labels and the client preference of §3.8 | With the implementation | CLI, then [clients](../clients.md) |
| `sessions.launch` creates a Session whose origin is `internal` | Its contract states no creation record | One row in §6.1 of the sessions service | With the implementation | Agents |

## 7. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (a command changes another owner's state) | Nothing is commanded. The creation entry states the origin of the Session it is itself creating, in the store call that creates it |
| §4 rule 2 (handover) | None: no handover changes |
| §4 rule 3 (before callbacks) | None |
| §4 rule 4 (no business meaning in core) | No core change. The column belongs to SessionStore's own Session record, as the leader link does |
| §4 rule 5 (batch) | The origin is written in the existing creation transaction; no batch and no event is added |
| §4 rule 6 (model context) | Unchanged: no event, so no context path, and the origin is never rendered into a request |
| §4 rule 7 (public events and services) | Agents answers from SessionStore's stored row, as `session.list` does; no plugin reads another's state |
| §4 rule 8 (infrastructure unchanged) | The Timeline, Dispatcher, envelope, assembly and the Session facade are unchanged. The store gains a column and no event, by the storage page's own format decision |
| §4 rule 9 (only facts later decisions use enter the Timeline) | The class is picker display data and a process-level record, so it is written through SessionStore's typed creation write and stays out of the Timeline |
| §1 (a feature is split by its state's owner) | The state is process-level and SessionStore's; no part is added to any scope |
| ADR 002 D2 (no wiring module) | No new plugin: SessionStore, Agents, Rpc and the existing creation entries carry it |
| ADR 002 D5 (language-neutral contracts) | §3 is the contract as data; the Rust spelling is §8 |

## 8. Binding appendix

- `SessionOrigin` — `client`, `acp`, `internal`, `unknown`, serde lowercase, one definition in the protocol crate, decoded
  from the store column into the same type.
- `SessionInfo.origin: Option<SessionOrigin>` and `SessionInfo.parent_session_id: Option<String>`, both
  `skip_serializing_if = "Option::is_none"`, filled only for protocol v3.
- `Create.origin: Option<SessionOrigin>`; the RPC boundary refuses the two values a client may not declare.
- `Catalog.with_acp_sessions: bool` and `Catalog.with_internal: bool`, default `false`, both in the cursor digest;
  `session.summaries` reads the same store query with both set to `true`.
- `hello.features` names: `session.create.origin`, `session.catalog.origins`.
- Store spelling: `agent_sessions.origin` and the existing `agent_sessions.leader_session_id`; the format decision is in
  [the storage page](native-storage-format.md#557-v30-格式决定).

## 9. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | One durable column on the stored Session record | A field on `SessionConfig`; a column only on `session_catalog` | `SessionConfig` is a runtime type every plugin can read, and a fork copies its text, so a fork would inherit its source's ACP label or force an in-transaction JSON rewrite. The catalog projection is derived from events, and a creation input is not; a projection-only column could not be rebuilt |
| D2 | The origin and the parent are two fields | One classification string such as `child` | A member is internal *and* has a parent; a launched Session is internal without one. One string would have to encode both and would lose the parent |
| D3 | `unknown` is a value | Absent or `null` for an unmapped Session | Absence already means "this reply carries no classification" (protocol v1 and v2, no store). Conflating the two would make an imported Session look like a protocol detail |
| D4 | The default list is `client` and `unknown`; `acp` and `internal` are opt-in | Defaulting to `client` alone; keeping today's unfiltered default | Filtering `unknown` out would hide a user's imported history from the only list they have; keeping the unfiltered default would keep the crowding this issue is about |
| D5 | A fork's origin is `client` | Copying the source's origin | A fork is a new Session of the client that forked it; the legacy catalog excluded forks from its ACP fallback for the same reason |
| D6 | The origin is written once, and an import may replace `unknown` once | Letting any later read or attachment update it | A Session's later attachments must not change its creation provenance, and a re-run of an import must be able to name what the source recorded without overwriting a value that is already known |
| D7 | `session.list` and `session.summaries` report the fields but filter nothing | Applying the same filters there | They are not the picker: `session.summaries` is the fallback list of older clients and the source of ACP's `session/list`, and `session.list` has no page boundary to keep |
| D8 | No parent filter and no children listing | A `parent` filter on the catalog | A live team comes from Collaboration's Team status, and a historical child is reachable by id and by `withInternal`; a parent filter would be an unused method surface until a client asks for it |
| D9 | No new index | A partial index per hidden class | Two independent switches need two indexes, each maintained by every commit that moves `updated_at_ms`; the filtered page is bounded by the page plus the rows it skips ([#596](https://github.com/arcships/dim/issues/596)) |

## 10. Acceptance (flow tests)

- A Collaboration leader and its member: the member lists as `internal` with `parentSessionId` equal to the leader and
  is absent from a default catalog page; `withInternal: true` lists it; attaching to it by id reports the same two
  fields without a catalog read.
- A Session created through `sessions.launch` lists as `internal` with no parent and is absent by default.
- A Session created with `session.create {origin: "acp"}` lists as `acp` and is absent by default; with
  `withAcpSessions: true` it is listed and labelled; a client that attaches to it afterwards does not change its origin;
  a fork of it lists as `client`.
- A client-created Session lists as `client`, and a Session imported with no mapping lists as `unknown`; both are on the
  default first page.
- `session.create {origin: "internal"}` is `invalid_params`; so is `"unknown"` and any other value.
- Filters apply before the page boundary: with 60 internal Sessions newer than 50 conversations, a 50-row page holds 50
  conversations, and paging by the returned cursor returns the rest exactly once in the stated order.
- A cursor taken under `withAcpSessions: true` and replayed with `false` is `invalid_params`; the same for
  `withInternal`. A cursor taken with both flags takes its page correctly.
- `session.summaries` still returns every stored Session, including internal and ACP ones, with the two new fields.
- Protocol v2 `session.list` items carry neither field; v3 items carry them.
- A v29 store with one member Session and one ordinary Session upgrades to v30 with the member reading `internal` and
  the ordinary Session `unknown`; the upgraded structure, marker and business rows are as the storage step states, and
  re-opening does not upgrade again.
- An `EXPLAIN QUERY PLAN` of the default page shows the activity index and no temporary b-tree, so the filter prunes in
  the index order rather than sorting the store.
- The picker against a daemon without `session.catalog.origins` sends today's request and draws today's rows; with the
  marker it sends `withAcpSessions` from its preference and draws the labels.

## 11. Changes to other documents

- [Client Session catalog](session-catalog.md#31-sessioncatalog): the two filters in the parameter table, in the cursor
  digest and in the acceptance list, and `session.summaries` reading the same query with both filters on.
- [Native storage format](native-storage-format.md#557-v30-格式决定): the column, the v30 step, the version lists and the
  table reference.
- [Migration](../plugins/Migration.md): which legacy setting and relation becomes which origin and parent, and the
  incremental reconciliation of already imported Sessions.
- [AcpGateway](../plugins/AcpGateway.md#a5-client-labels): A.5 stops reporting the missing field and describes what
  `session/new` sends.
- [Clients](../clients.md#34-system-management): the `/settings` row for ACP Session display.
- [CLI commands](../cli-commands.md) and the [native CLI contract](../../../crates/dim-cli/README.md): `/resume`'s
  filters and labels and the `/settings` switch, once the client implements them.
- [Agents sessions service](sessions-service.md#61-launch): the origin a launched Session records.
- [Overview](../overview.md#b2-app-server-process-plugins): the Rpc row's `session.create` parameters.
- [Documentation index](../../INDEX.md) lists this page.
