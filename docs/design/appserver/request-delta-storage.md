# Request delta storage (native format v17)

Status: Implemented in [#224](https://github.com/arcships/dim/pull/224), native format v17. Implementation corrected
four statements, each where it applies: preparation also serializes the new nodes (§6.1), a request row serializes its
event without the request content (§6.1, §6.3), one reader serves a whole validation pass (§7.1, §7.2), and acceptance
thresholds are measured rather than asserted (§18). Phase 2 of
[#193](https://github.com/arcships/dim/issues/193), as agreed in the issue's latest comment. Phase 1, a bounded content
cache shared across the pages of a read, merged as [#213](https://github.com/arcships/dim/pull/213); phase 3 is a
separate runtime proposal (§15). Owner lane: Agents/storage (yyhhyyyyyy). Reviewers: zerob13, eric8810. Scope: inside
the native `TimelineStore` implementation (`SqliteTimelineStore`,
[timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs) and
[codec.rs](../../../crates/dim-app-server/src/storage/codec.rs)) and the [storage format](native-storage-format.md),
which this design moves to v17. This revision replaces the first draft's deltas between request rows with message lists
in the content table (§16 D1, §17).

## 1. Summary

In format v16 a `context/prepare` or `model/call` row stores one content-hash reference per message of its request.
Each request repeats the previous one and adds a few messages, so the k-th model call stores k references. Storage,
write hashing and read decoding all grow with the square of the number of model calls between compactions (§2).

- **Rule.** v17 keeps v16's split between event rows and the Session's content rows and changes only how a request
  row references its messages: one reference to a list instead of one per message. A list node is a content row that
  holds one message and the content address of the node before it, so requests that share a message prefix share its
  nodes. A request row stores the address of its last node and the list length, next to v16's `tools` and `system`
  references. Writing a request adds a node for each message after the prefix it shares with the previous request on
  its branch (§5).
- **Deterministic.** Nodes are content-addressed, and the bytes are a function of the branch's logical history and the
  event. A per-branch writer memo only spares reading the previous request back (§6).
- **Reads.** A request is rebuilt from its own row and its own list, never from another event row. A read keeps the
  last request it rebuilt and loads only the nodes that request lacks; a request read cold loads one node per message
  (§7). A list walk releases the connection after at most 256 node loads. A checkpoint read from `resume` therefore
  reads no request row before `resume`, only the rows today's read returns (§10). A missing or damaged node fails the
  read; a request is never restored empty (§8).
- **Measured.** In the mock, tool and product-window profiles every request outside a compaction extends the previous
  one, and the previous request is always still in memory when the next one is written (§4). At about 10,000 events
  v16 stores 13,778 bytes per event and v17 would store 411, about what it stores at 1,000 events (plain mock turns;
  §12).
- **Format.** v17 is an in-place marker bump from v16 in the existing admission transaction, with no table or row
  rewrite; `timeline_content` gains the kind `node`. v16 rows stay readable for good, and v16 and v17 rows mix in one
  database (§11).

Event types and their frozen fixtures, the Timeline, the Dispatcher, the `TimelineStore` trait and its read semantics,
capabilities and external protocols do not change; §3 names the mechanism that keeps each storage convention. No new
event, service, method, wire field, configuration or runtime requirement.

## 2. Problem and evidence

### 2.1 Restore cost (#193)

Release build, Apple M4, `SqliteTimelineStore` on a disposable database, `defaults::mock_hosts` and
`defaults::install`. History was grown through mock-model turns (a plain turn writes 10 events, a tool turn 18); seven
restores per size. Timed: `Agent::restore_session` plus the first `idle("main")`.

| Events | Plain, median | Tools, median |
| --- | --- | --- |
| ~1,000 | 65 ms | 80 ms |
| ~3,000 | 327 ms | 404 ms |
| ~10,000 | 2.77 s | 3.48 s |

At 10,017 plain events, 2,186 ms of the 2,772 go to 11 full-history reads (about 199 ms each) and 549 ms to
`validate_session`. Each request row stores one reference per message (`encode_refs`), so at 10k events the rows hold
2,000,000 references and 137.5 MB of event rows for 2,001 distinct contents, and decoding resolves every reference
through `ContentCache`.

### 2.2 Phase 1 (#213)

Phase 1 resolves a reference with one `foldhash` lookup and shares one cache, bounded at 64 MiB of bodies, across
the pages of a read and across a `validate_session` pass. The writer and the written bytes are unchanged. Its results
(mean of two interleaved rounds, each the median of seven restores):

| Mix | Events | Restore + first idle | Per read | `validate_session` |
| --- | --- | --- | --- | --- |
| plain | ~1,140 | 71 → 58 ms | 5.0 → 4.3 ms | 9.7 → 5.3 ms |
| plain | ~3,140 | 344 → 245 ms | 25.2 → 19.6 ms | 58.6 → 22.1 ms |
| plain | ~10,140 | 2,866 → 1,779 ms | 208 → 146 ms | 576 → 149 ms |
| tools | ~1,140 | 80 → 71 ms | 5.7 → 5.3 ms | 11.5 → 6.5 ms |
| tools | ~3,130 | 417 → 303 ms | 30.5 → 24.3 ms | 73.3 → 27.8 ms |
| tools | ~10,140 | 3,624 → 2,256 ms | 257 → 184 ms | 724 → 186 ms |

### 2.3 What remains

Fitting Phase 1's plain per-read times to `a·E + b·R`, with `E` events and `R` references (`2T² + 4T` for `T` plain
turns), gives a ≈ 2.9 µs per event and b ≈ 58 ns per reference; the fit reproduces the 1,140-event read (4.5 ms against
4.3 ms). At 10k events the references take 117 of the 146 ms of each read. Only a format change removes them.

Writes pay too. For each request row the v16 writer serializes and SHA-256-hashes every message, the tools and the
system prompt, and runs one `INSERT OR IGNORE` per content. Over the last 200 request rows of a 10k-event plain Session
that is 542 µs per row before any SQL, against 13 µs for the v17 encoding, prefix comparison included (§6.3). Each
model step also reads its context history twice
([timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#4-读取)), and without compaction those reads
decode every earlier request.

The storage checks done when the store was built used frequent compaction, so request lists stayed short. Many model
calls between compactions fall outside that envelope: the mock and `--models` profiles never compact, and with a large
window a token-based compaction can come hundreds of steps apart.

## 3. Constraints

Each row names a convention, where it is written, and the mechanism by which v17 keeps it.

| Constraint | Source | How v17 keeps it |
| --- | --- | --- |
| Events are written and read as their original types; encoding exists only inside storage | [timeline-storage §2](../../architecture/rust-agent/timeline-storage.md#2-存储编码) | A read rebuilds the complete `ModelRequest` (`messages`, `tools` and `system` held by `Arc`, as today) from its row and the content rows it reaches; the list format exists only in `codec.rs` and the stored JSON |
| Request content is stored by content hash in the Session's content table, each content once, and the event body keeps only content addresses ("请求内容" row) | [timeline-storage §2](../../architecture/rust-agent/timeline-storage.md#2-存储编码) | Messages become list nodes in `timeline_content`, addressed by v16's `SHA-256(kind NUL body)` rule and stored once per Session; the event row keeps the last node's address, the length, and the `tools` and `system` addresses. The unit of sharing changes from a message to a message after a given prefix; §19 rewords that row with the implementation |
| One decoded-content cache per read call or validation pass, at most 64 MiB, released at the end ("读取" row) | [timeline-storage §2](../../architecture/rust-agent/timeline-storage.md#2-存储编码) | Node loads go through that cache. Beyond it a read keeps one rebuilt request and its node addresses (§7.1) |
| The request stays in the events: `model/call` carries the final request handed to the model | [ADR 005 D1](../../adr/005-observability.md#d1-two-observation-layers) (the Timeline holds "final model requests (`model/call`)") | The request is neither derived nor loaded lazily |
| ContextPolicy reads the latest exact request | [context_policy.rs](../../../crates/dim-agent-capabilities/src/context_policy.rs) `ContextPolicy::estimate` compares provider, model, capability snapshot, tools, system, config and metadata with the previous `model/call`, and requires `messages.starts_with` its messages | Decoded requests equal the written ones (§18 acceptance 1) |
| Event types and the frozen fixtures `crates/dim-agent-types/tests/fixtures/events.json` and `events.ts` with the `*_contract.rs` tests | [principles §6](../../architecture/rust-agent/principles.md#6-验证), AGENTS.md rule 6 | Untouched |
| The Timeline (sequence, atomic batches, commit and publish, before), the Dispatcher (hot window only), the `TimelineStore` trait and its read semantics, and capabilities rebuilding from `local(..=S)` | [timeline-storage §1, §3, §4](../../architecture/rust-agent/timeline-storage.md), [time bus](../../architecture/rust-agent/time-bus.md), [single tree](../../architecture/rust-agent/single-tree.md) | No code outside the store changes. Every read returns the same events, in the same order and batches, with the same errors for intact data; only storage errors for damaged request content have new messages (§8) |
| A checkpoint read "does not scan request bodies before the snapshot recovery position" ("不扫描快照恢复位置之前的请求正文") | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描), bounded reads table | A request row is rebuilt from its own references and the content rows they reach, never from another event row, so the read's event rows start at `resume` as today. The content it loads is the messages of the requests it returns, which v16 loads through the same rows' references (§10, acceptance 3a) |
| Reads go in segments and release the connection after each; no lock hold decodes unbounded history ("每段结束释放连接锁 … 不在锁内解码无限历史") | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | Event rows are read in today's segments (256 sequences, 64 rows in validation). A list walk releases the connection after at most 256 node loads and continues from the next address; the read call keeps the partial walk between holds, as #213 keeps its cache (§7.2, acceptance 3a) |
| Restore validates each page with its references and releases the lock per page; its caches are bounded ("恢复" row) | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | `validate_session` checks every list in the same holds, through the pass's cache (§8) |
| Serialization, reference resolution, hash checks and type conversion stay at the storage boundary | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | Every node is hash-checked when it is loaded, as content rows are today (§8) |
| External protocols expand to the original events ("外部协议仍展开为原事件") | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | Untouched |
| An incompatible change needs a new version and a read path; an upgrade only adds structures and reads or rewrites no business row; one-way | [Storage format, 格式版本](native-storage-format.md#格式版本) and the S5a/S5b decisions | v17 by a marker bump (§11) |
| Serialization optimizations measure body copies, encode counts and time, and add no business state | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | Measurements in §4, §6.3, §7.4 and §12; the writer memo holds no business state and decides no byte (§6.2) |
| Idle branch live bytes do not grow with steps | [timeline-storage §7](../../architecture/rust-agent/timeline-storage.md#7-验收) | The memo holds one fixed-size entry per branch, with no request and no node list; a read holds one request and its node addresses beyond what it returns (§6.2, §7.1) |
| A fork copies only the content rows its copied events reference | [S5c §6.3](session-deletion-and-fork.md#63-what-a-fork-copies) | The copy follows each copied list to its first node (§9) |

## 4. Prefix stability

### 4.1 Why consecutive requests extend each other

Sharing pays only if each request starts with the one before it. Three existing contracts make that hold between
compactions:

- **The projection is append-only.** Events enter model context only through adopted inputs and tool results
  (AGENTS.md rule 10, [principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 6), and
  the history renderer places each at the end. A `context/prepare` request is the rendered history at its `through`.
- **Preparation may only append.** ContextPreparation rejects a prepared request that does not start with the rendered
  history ("Context preparation must preserve the timeline history prefix; additional messages may be appended",
  [context_preparation.rs](../../../crates/dim-agent-capabilities/src/context_preparation.rs)). A `model/call` request
  therefore extends its `context/prepare` unless ContextPolicy compacted in between.
- **ContextPolicy already relies on it.** Its provider baseline applies only while `request.messages` starts with the
  messages of the previous `model/call`.

A completed compaction replaces the head of the list (replacement, baseline, retained tail), so the request after it
shares no prefix with the previous one and starts a new list (§10). A change of `system` or `tools` (a prompt revision,
a tool adoption) writes one content row for the new value.

### 4.2 Measured

A throwaway probe (not committed) wraps `SqliteTimelineStore` in a forwarding `TimelineStore` over a disposable
database. At every `append` it compares each request with the previous request on its branch: the common prefix by
`Arc::ptr_eq` and then `==`, `system` and `tools`, and whether the previous request's `Arc` is still alive, which is
what a `Weak` memo would see. It also builds the exact v17 nodes and references. Release build, Apple M4. Profiles:

- **plain** and **tools**: `defaults::mock_hosts` and `defaults::install` with the turns of
  [`restore_cost`](../../../crates/dim-app-server/examples/restore_cost.rs); no compaction.
- **product window**: the product window facts (`ProductContextModel`) and summary strategy
  (`ProductSummaryStrategyPlugin`), `IdentityContext`, `HistoryRenderer::default()`, `MockTools`, and a capability
  snapshot with a 32,000-token window and 2,000 output tokens. The model driver echoes like `EchoModel` and answers the
  summarizer with a short summary that differs each time. Turns alternate plain text and `tool:echo`, each carrying
  1,000 bytes, so the window fills and compacts about every 20 turns.

Every request row is counted:

| Profile | Events | Request rows | New lists (completed compactions) | Rows keeping the previous request's full length | Kept / total messages | New nodes per row | Previous request alive at append |
| --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 1,001 / 3,001 / 10,001 | 200 / 600 / 2,000 | 1 (0) | 100% | 99.0% / 99.7% / 99.9% | 1.00 | 100% |
| tools | 1,009 / 3,007 / 10,009 | 224 / 668 / 2,224 | 1 (0) | 100% | 99.1% / 99.7% / 99.9% | 1.00 | 100% |
| product window | 3,007 / 10,017 | 578 / 1,926 | 10 (9) / 33 (32) | 98.4% / 98.3% | 97.4% / 97.4% | 1.45 / 1.48 | 100% |

"Rows keeping the previous request's full length" counts every request row except the first; in the product window
the remainder is exactly the 9 and 32 rows right after a completed compaction. Per row, a `model/call` adds no node, a
`context/prepare` adds two (the model output and the next input or tool result), the first request one, and the
request after a compaction ten in this profile.

- Outside the request right after a compaction, every request extends the previous one: `keep` equals its length.
  Each new list is the first request of the Session or the request right after a completed compaction.
- A `model/call` keeps its `context/prepare`'s message `Arc`s, so its prefix is found by pointer. A `context/prepare`
  shares no pointer with the previous `model/call`: the renderer builds new messages each step, so its prefix is found
  by `==`. The value comparison is part of the v17 write cost in §6.3.
- In the plain and tool profiles `system` and `tools` never changed after the first request.
- At every request append, including the first request of a turn after the Session went idle, the previous request
  on the branch was still alive. The probe observes liveness, not the holder; ContextPreparation and ContextPolicy
  each keep their latest prepared request, and the Timeline keeps recent events in its hot window. A `Weak` memo
  therefore always hits in a running Session.
- No run had a request in a before trace.

### 4.3 Not measured

- Real provider adapters (`ModelProviderPlugin::process`, aimux) and a non-identity preparation host: the profiles
  use `IdentityContext` and an echo driver. A host that appends per-request messages, which the preparation rule
  allows, adds nodes for those messages in each `model/call` row.
- Prompt-source revisions and tool adoption during a Session: each writes one `system` or `tools` content row by
  construction.
- Before callbacks on request events: the product assembly registers none (only tests do, on `model/call`). Drafts
  are covered by the acceptance tests (§18).
- TypeScript plugins and the CLI product profile with a real model.

**Assumption.** Consecutive requests on a branch extend each other between compactions. If a producer rewrote an
early message every step, each request would add a node for every message from the first changed one, so storage per
row would again grow with the request: by message bytes, where v16 grows by 67 bytes per reference. Reading one request
cold would still load exactly its own nodes (§7.4). Nothing in the product rewrites early messages today. It would also
defeat provider prompt caching and ContextPolicy's baseline, and ContextPreparation already refuses it from the
preparation host. The acceptance in §18 measures bytes per event so a regression shows.

## 5. Encoding

### 5.1 Lists and the previous request

- **Request event**: a `context/prepare` or `model/call` event; its `data.request` is the request. A draft in a
  before trace is never the previous request of another record.
- **Node**: a `timeline_content` row of kind `node` whose body is `{parent, message}`. `message` is one message in the
  v16 `message` body form (`{role, content}`); `parent` is the address of the node of the message before it, or `null`
  for a first message. Its address is v16's `SHA-256(kind NUL body)`. A node therefore identifies a whole message
  prefix, and requests with the same prefix reference the same nodes.
- **List** of a request: its messages as nodes, referenced by the address of the last node and the length. A request
  with no messages references no node.
- **Logical history** of branch `b`: the ancestor prefixes up to their cuts, then `b`'s own events. This is the
  ancestry `[(branch_id, cut)]` the runtime builds from the branch directory, which the store holds as `parent` and
  `branch_point` in `timeline_branches.info`.
- **Previous request** of a request event at sequence `s` on `b`: the last request event before it in `b`'s logical
  history, that is the last one on `b` before `s`, else the last one at or before the cut on the nearest ancestor that
  has one.
- **keep**: the length of the longest common prefix of the two `messages` lists. Each pair is compared with
  `Arc::ptr_eq` first, then with `Message`'s `==`.
- **Writing a list**: the first `keep` messages reuse the previous request's nodes; each later message gets a new node
  whose parent is the node before it. A request with `keep` 0 (the first request, the request after a compaction)
  starts a new list. Nodes are written with `INSERT OR IGNORE`, as v16 writes content.
- **`tools` and `system`**: absent → `null`; equal to the previous request's value (pointer, then `==`) → the previous
  request's address, without serializing; otherwise serialized, hashed and written as a v16 content row of kind
  `tools` or `system`.
- **Drafts**: a request in the record's before trace (`WireTrace.original`, each non-null `WireStep.result`) is
  encoded the same way, with the record's event request as its previous request. That request always exists, since a
  draft has its event's kind.

The writer always takes the immediately previous request and the longest prefix, and nodes are content addresses, so
the bytes are a function of the logical history and the event. Equality is the types' own `==`, which is also how the
storage acceptance compares events (§18). One consequence: a JSON object inside a message that is equal in value but
serialized with a different key order counts as kept, keeps the earlier node and reads back in that node's key order.
Consecutive requests render their messages from the same events, so this does not happen in practice, and no reader
depends on key order.

Nothing bounds a list, and nothing needs to: a list holds exactly its request's messages, so rebuilding a request never
reads anything but its own row and its own nodes (§7.4).

### 5.2 Persisted JSON types

The v16 `Row`, `WireTrace`, `WireStep` and `References` are unchanged. v17 adds a reference shape for request records
and a content kind:

```text
WireEvent      = { event: AgentEvent, refs: References | ListReferences | null,
                   trace: WireTrace | null, batch_end: integer }
WireDraft      = { event: EventDraft, refs: References | ListReferences | null }
References     = { messages: string[], tools: string | null, system: string | null }      v16
ListReferences = { list: string | null, length: integer,
                   tools: string | null, system: string | null }                          v17
node body      = { parent: string | null, message: Message }                    content kind "node"
```

- A v17 writer writes `ListReferences` on every `context/prepare` and `model/call` record and on their request
  drafts. v16 request records keep `References`. The two shapes are told apart by their fields, and each rejects
  unknown ones. Every other record keeps `refs: null`, so its bytes are exactly what v16 writes.
- As in v16, the stored `data.request` has `messages: []` and no `tools` or `system`; its other fields stay in place.
- `list` is the address of the last node, and `null` exactly when `length` is 0.
- `tools` and `system` are v16 content addresses, as in `References`.
- Addresses carry no Session id, so a fork's byte copy stays valid (§9).

### 5.3 Example

Three request rows of a Session and the content rows they reach: the first prepare on `main`, the next prepare on
`main`, and the first prepare of a child cut at `main` 10. Displayed as JSON; the column stores the string. Content
addresses are shortened to `s1` and `n1` to `n6`; each is stored as a 64-character SHA-256 hex string.

```json
[
  {"branch_id": "main", "sequence": 4, "kind": "context/prepare", "body": {"encoding": "event", "record": {
    "event": {"id": "e4", "sequence": 4, "create_at": 1004, "type": "context", "name": "prepare", "data": {
      "turn_id": "t1", "request_id": "r1", "through": 3,
      "request": {"providerId": "mock", "modelId": "deterministic", "messages": []}}},
    "refs": {"list": "n1", "length": 1, "tools": null, "system": "s1"},
    "trace": null, "batch_end": 4}}},
  {"branch_id": "main", "sequence": 9, "kind": "context/prepare", "body": {"encoding": "event", "record": {
    "event": {"id": "e9", "sequence": 9, "create_at": 1009, "type": "context", "name": "prepare", "data": {
      "turn_id": "t2", "request_id": "r2", "through": 8,
      "request": {"providerId": "mock", "modelId": "deterministic", "messages": []}}},
    "refs": {"list": "n3", "length": 3, "tools": null, "system": "s1"},
    "trace": null, "batch_end": 9}}},
  {"branch_id": "child", "sequence": 3, "kind": "context/prepare", "body": {"encoding": "event", "record": {
    "event": {"id": "c3", "sequence": 3, "create_at": 1013, "type": "context", "name": "prepare", "data": {
      "turn_id": "t3", "request_id": "r3", "through": 2,
      "request": {"providerId": "mock", "modelId": "deterministic", "messages": []}}},
    "refs": {"list": "n6", "length": 4, "tools": null, "system": "s1"},
    "trace": null, "batch_end": 3}}}
]
```

```text
 hash  kind    body
 s1    system  "You are helpful."
 n1    node    {"parent": null, "message": {"role": "user", "content": "Hi"}}
 n2    node    {"parent": "n1", "message": {"role": "assistant", "content": "Echo: Hi"}}
 n3    node    {"parent": "n2", "message": {"role": "user", "content": "Again"}}
 n6    node    {"parent": "n3", "message": {"role": "user", "content": "Follow up"}}
```

`main` 5 and `main` 10 are the `model/call`s of `r1` and `r2`; each references the same list as its prepare and adds
no node. `main` 14, the next prepare, adds `n4` and `n5`.

```text
 main                                     child (parent main, cut 10)
  4 context/prepare  list n1, 1
  5 model/call       list n1, 1
  9 context/prepare  list n3, 3
 10 model/call       list n3, 3             3 context/prepare  list n6, 4
 14 context/prepare  list n5, 5             7 model/call       list n6, 4

 nodes   n1 [u1] <-- n2 [a1] <-- n3 [u2] <-- n4 [a2] <-- n5 [u3]
                                     ^
                                     +------ n6 [u4]
```

The child's first prepare shares `n1` to `n3` with `main` and adds `n6`. It refers to no `main` event row, and the
nodes `main` adds after the cut are not in its list.

## 6. Writing

### 6.1 Finding the previous request

The writer thread collects a round of batches and then commits them in one transaction (`commit_round`). v17 splits a
request record's encoding in two. Before the transaction the writer prepares each request record of the round, in
order; inside it, it writes what the preparation decided. For a request record at `(b, s)`:

1. **Earlier in the round, or memo hit.** If an earlier record of the round on `b` is a request, the last such record
   is the previous request, in memory. Otherwise, if the memo (§6.2) has an entry for `b` whose request is still
   alive, that entry is `b`'s last request row: its request, list address and length, and `tools` and `system`
   addresses.
2. **Otherwise** the writer reads it through the store's ordinary read path: the last request row on `b` before `s` (a
   backwards primary-key scan that reads the `kind` column, not the body), else the last one at or before the cut on
   each ancestor in turn, using the parents and cuts in `timeline_branches.info`, or for a branch created earlier in the
   round its branch-create record. It rebuilds that request by walking its list (§7.2). A v16 previous row has no nodes
   to share, so the writer does not rebuild it: the new request writes a node for every message and its `tools` and
   `system` as v16 does, once per branch (§11).
3. **No request found**: `keep` is 0.

Then the writer computes `keep` in memory. The parent of the first new node is the previous request's node at position
`keep − 1`: its last node when `keep` equals its length, as in every measured row outside a compaction, otherwise
reached by stepping back `length − keep` parents from it. The same steps prepare each request draft against the
record's event request.

Every node load of the preparation goes through the read path, in connection holds of at most 256 node loads (§7.2). The
writer thread is the only thread that changes an existing Session's rows (appends, creation rollback and delete all run
on it; a fork writes a new Session), so the committed rows it read are still current when its transaction starts. The
preparation also serializes and hashes each new message into its node, and a changed `tools` or `system` (§5.1). Inside
the transaction the writer writes the nodes with `INSERT OR IGNORE`, then the changed `tools` or `system`, then the row,
serialized from a copy of its event whose request carries no content. The transaction loads and serializes no node and
no request content, and its work per request row follows the new messages only.

### 6.2 Writer memo

One entry per `(Session, branch)`: the sequence of the last request row the writer encoded on that branch, a
`Weak<ModelRequest>` to its request, its list address and length, and its `tools` and `system` addresses. The entry
has a fixed size.

- The memo holds no request: when the runtime drops the request, the entry is dead. Dead entries are removed at the
  end of each write round, so the memo holds one small entry per branch whose last request is still in memory.
- A transaction that fails or rolls back removes the entries of its Sessions, and so do `rollback_session` and a
  delete. A removed entry only costs the next write the lookup of §6.1 step 2.
- The memo decides no byte. On a hit the previous request is the one in memory; on a miss it is rebuilt from the
  store. The comparison is by value in both cases, kept messages take the previous request's nodes, and new nodes are
  the content addresses of what is written, so both produce the same bytes (§18 determinism). It is storage-internal
  acceleration, not business state.

Measured (§4.2): the previous request was alive at every request append in every profile, so a running Session writes
without reading. After a restart, an unload or a fork, the first request write of each branch rebuilds the previous
request before its transaction, once per branch; §7.4 gives its cost.

### 6.3 Work per request row

| | v16 | v17 |
| --- | --- | --- |
| Serialize | The whole event, request included, then every message, tools and system | The event without its request content, the new messages as nodes, and a changed `system` or `tools` |
| Hash | SHA-256 of each serialized content | SHA-256 of each new node and of a changed `system` or `tools` |
| SQL | One `INSERT OR IGNORE` per content, then the row | One `INSERT OR IGNORE` per new node and changed value, then the row |
| Compare | None | Prefix with the previous request: by pointer for `model/call` rows, by `==` for `context/prepare` rows (§4.2) |
| Measured before SQL, content and comparison only, median of the last 200 request rows at ~10k events | plain 542 µs, tools 979 µs, product window 462 µs | plain 13 µs, tools 24 µs, product window 23 µs |

In the measured profiles a `context/prepare` row adds two nodes and a `model/call` row none (§4.2). Most of the v17
time is the value comparison of a `context/prepare` row's prefix: at 10k plain events those rows take 15 µs and
`model/call` rows, compared by pointer, 1.7 µs. Output-row merging and every other record are unchanged.

The measured times leave out the row itself. v16 serializes the whole request into a JSON value and then replaces its
content with references, so that copy grows with the request. v17 serializes the row from a copy of its event whose
request carries no content (§6.1).

## 7. Reading

### 7.1 In order

Each read keeps the last request it rebuilt, with the node address of each of its messages: one per call of
`read_segments` or `read_page` and one per `validate_session` pass, carried across the segments and pages of that call
and the branches of that pass, as #213 carries its cache. Keeping it across branches is safe because a node's address
names the whole message prefix that ends at it: an address that matches at the same position is a shared prefix,
whichever branch the kept request came from. For a v17 request row the read walks the list from its last node towards
its first. Before loading a node it compares the node's address with the kept request's address at the same position; at
the first match the rest of the list is the kept request's, and the walk stops. The rebuilt request is the kept
request's messages up to that position followed by the loaded messages, and its `tools` and `system` load through the
cache by address. The kept request is then truncated and extended in place, and the event receives its own message
vector, as the event type requires (§7.4).

- A v16 request row decodes through `refs` and #213's bounded content cache, as today. The kept request then has no
  node addresses, so the next v17 row walks its whole list.
- Drafts are rebuilt the same way against their record's request and do not replace the kept request.

A read that follows the logical history (`context`, `history`, full `local` and restore reads) therefore loads each
node once, and only the first request of a call walks its whole list.

### 7.2 One request

A request with no kept request to match walks its whole list: one node load per message, each message materialized
once from the last to the first, and no other request touched. Each node is checked when it is loaded (§8). The walk
releases the connection after at most 256 node loads; the read call keeps the messages gathered so far and the next
address and continues in the next hold, as #213 keeps its cache between pages, so a concurrent writer waits at most one
hold. `tools` and `system` are one load each.

This happens once per read call, for its first request: a page that starts mid-history, a read by event id, `trace`, a
snapshot anchor, a checkpoint suffix read from its `resume`, a branch's own segment (read apart from its ancestors). A
validation pass checks branches in id order, so a branch's first request walks until it meets the request kept from the
branch checked before it. After its in-order pass it reads the preparation boundary row of each completed compaction
again; #213's cache holds those nodes unless it was cleared in between. A writer memo miss does the same walk, before
its write transaction (§6.1). `read_page` checks its two caller-supplied boundaries with separate reads, as v16 does
([timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs) `read_page`), so a page call rebuilds at most
three requests this way: its boundary rows when they are request rows, and its first request.

### 7.3 Read path

```text
 read call (read_segments, read_page, a validation pass, read by id, trace)
   kept = none
   for each row, in sequence order, in today's segments:
     v16 request row  --refs--> content rows (#213 cache) --------------------------> request --+
     v17 request row  --list: last node -> parent -> ...                                       |
                        stop at the kept request's node at the same position, or at the first   |
                        at most 256 node loads per connection hold, through the cache          |
                      --kept.messages[..p] + loaded messages; tools, system by address --> request --+
                                                                   kept = request, in place <--+
     drafts of the row: the same walk against their record's request
```

### 7.4 Cost, and why there is no bound

A list holds exactly its request's messages. Rebuilding one request cold loads one node per message and nothing else,
whatever came before it: no other request row, no message that a later request dropped, no `model/call` or retry row
that added nothing. There is nothing to bound, so v17 has no depth limit and no periodic full copy.

Measured on the probe's databases (§12), median of five; the request is the last `model/call` of the Session, read
cold, and the v16 column is the store's own read of that event on `main`:

| Profile | Events | Messages and node loads | v16 read of the event | v17 list walk (prototype) |
| --- | --- | --- | --- | --- |
| plain | 1,001 | 199 | 0.55 ms | 0.89 ms |
| plain | 3,001 | 599 | 3.0 ms | 2.6 ms |
| plain | 10,001 | 1,999 | 9.7 ms | 14.4 ms |
| tools | 1,009 | 223 | 0.62 ms | 1.0 ms |
| tools | 3,007 | 667 | 3.4 ms | 3.0 ms |
| tools | 10,009 | 2,223 | 11.3 ms | 16.2 ms |
| product window | 3,007 | 54 | 0.53 ms | 0.38 ms |
| product window | 10,017 | 69 | 0.76 ms | 0.75 ms |

Both load one content row per message, and the walk took between 0.7 and 1.65 times v16's read of the same request: a
node is larger than a v16 message row (it carries its parent's address), and the prototype loads it with a plain
query. In the product window a request is bounded by the window, whatever the history.

Synthetic lists separate the request's size from the history before it. A throwaway program writes the same chain in
three forms on disposable databases (release, Apple M4, SQLite's default page cache of 2 MiB, median of seven): v16
references for the last request, the first draft's deltas between request rows (§17), and v17 lists. Each turn is a
`context/prepare` that adds two 100-byte messages and a `model/call` that adds none; in the partial chains each prepare
also rewrites the last four messages, and in the adversarial chain the last half. Times rebuild the last request cold:

| Chain | Messages in the last request | v16 references | First draft, one recursive query, oldest first | First draft, backwards, 256 rows per hold | v17 list, 256 nodes per hold |
| --- | --- | --- | --- | --- | --- |
| Append-only | 1,999 | 4.4 ms | 7.8 ms | 4.6 ms | 3.8 ms |
| Append-only | 19,999 | 97 ms | 598 ms | 57 ms | 100 ms |
| Last four rewritten | 1,999 | 4.5 ms | 9.3 ms | 5.6 ms | 5.6 ms |
| Last four rewritten | 19,999 | 96 ms | 630 ms | 64 ms | 104 ms |
| Last half rewritten | 999 | 3.9 ms | 70 ms | 26 ms | 3.5 ms |

| Append-only, 19,999 messages, a writer wanting the connection every 2 ms | Longest connection hold | Longest wait of the writer |
| --- | --- | --- |
| First draft, one recursive query | 598 ms | 592 ms |
| v17 list, 256 nodes per hold | 1.7 ms | 1.3 ms |

- The first draft decoded its chain oldest first and built every intermediate request: about `N²/2` pointer copies
  for `N` messages, 199,980,001 at 19,999, which dominated its time and made one cold request quadratic.
- v17 loads exactly one node per message in every chain. The cost of a load rises from 1.9 µs at 2,000 messages to
  5.0 µs at 20,000, as v16's per reference does (2.2 to 4.9 µs): both look up content addresses, which lie in random
  order in the table's B-tree, so a larger table misses the page cache more often.
- The first draft's backwards walk steps through event rows in sequence order and stays near 2.8 µs a row at 20,000.
  It is faster there, but it reads other requests' rows, which a checkpoint read must not (§10), and its cost follows
  the history rather than the request: with half of each request rewritten it parsed every rewritten message again
  (26 ms at 999 messages, against 3.5 ms for the list).

The writer memo miss (§6.2) is the same walk, in the same holds, before the write transaction, once per branch after
a restart: 14 ms for the 1,999 messages of the plain 10k Session, 16 ms for the 2,223 of the tool Session, under 1 ms
in the product window.

One cost remains quadratic in the number of calls between compactions: every decoded request carries all its message
pointers, as the event type requires (`messages: Vec<Arc<Message>>`). A full-history read of 10k plain events builds
2,000,000 pointers, about 7.7 ms of the prototype's 48.0 ms (§12). v16 builds the same vectors; v17 removes the
lookups and parsing behind them. Phase 3 (§15) removes the repetition of whole reads during a restore.

### 7.5 Reads that do not use request bodies

A paging or projection read returns what an event says without rebuilding the requests stored with `context/prepare`
and `model/call`: the request in such a record keeps the empty message list and the absent `tools` and `system` that
its row holds, and no content row it references is loaded, hashed or checked. These readers never look at those requests:
history paging discards both kinds, `session.summaries` reads only input text and the last event's time, the context
projection projects input and output facts, and collaboration reads tool calls, results and turn outcomes. They page
through `TimelineStore::read_page_without_request_bodies`, whose default is `read_page`; a store may implement it
with no request walk at all. Every read that uses a request (`local`, `history`, `context`, `trace`,
`validate_session`, export) still rebuilds and checks each one as §7.1–§8 state.

- The read still checks that each record and each draft of its trace has the references its kind requires, and no
  other record has any; it never follows them.
- A damaged node therefore does not fail a paging read, because that read would not have loaded it. Restore and every
  read that uses requests still fail on it, and a Session with a damaged list is still refused at restore (§8).
- The streamed projection (§10) reads its history through paging, so the events it projects carry no stored request.
  The one request an estimate compares against comes from `policy_facts`, whose rows include the `model/call` of the
  last `model/usage`, not from the projected events.

## 8. Validation and damaged rows

For each request record a read that uses requests checks, failing with a storage error otherwise (a read that does
not use them checks only the first item, the reference shape, §7.5):

- a `context/prepare` or `model/call` record, and each of its request drafts, has `References` (v16) or
  `ListReferences` (v17); any other record has `refs: null`;
- `ListReferences`: `list` is `null` exactly when `length` is 0, and the walk from `list` visits exactly `length`
  nodes, the last one visited with `parent: null` and every other with a parent;
- every node is a content row of kind `node` whose address matches the SHA-256 of its body, and its body parses as
  `{parent, message}`;
- `tools` and `system` addresses resolve to content rows of their kind with matching hashes, as in v16;
- the rebuilt request parses as `ModelRequest`.

An in-order walk that stops at the kept request checks only the nodes it loads; the shared ones were checked when the
kept request was rebuilt. A missing or damaged node fails the read explicitly ("missing request content", "request
content hash mismatch", "request list length mismatch"); the request is never restored empty or partial. The bad row
keeps its bytes: reads never write. `validate_session` applies the same checks to every row, so a Session with a broken
list is refused at restore and preserved, as a missing content row is today. A damaged node reaches every request
whose list passes through it, which in normal operation is every request from the one that added it to the next
compaction: the requests that reference the same message in v16.

## 9. Branches, fork and delete

- **Across branches.** A child's first request shares the nodes of the last request at or before its cut on the
  nearest ancestor that has one; it references nodes only, never an ancestor event row. A parent that keeps writing
  after the cut adds nodes that no list of the child contains.
- **Fork** ([S5c §6.3](session-deletion-and-fork.md#63-what-a-fork-copies)). `main` rows through the cut are copied
  byte for byte. The fork copies the content rows those rows reference, as today, and for each `list` every node on
  the way to the first one: one recursive query over `(session_id, hash)` in the fork transaction, whose
  `UNION` visits a prefix shared by many lists once. No node of a request after the cut is copied unless an earlier
  request shares it. Copied child directories have head 0 and no events. The copy remains one transaction whose length
  grows with `main`'s copied prefix, as S5c states. The `session/forked` row the fork then writes at `at + 1` carries
  no request and references no content, so the closure copies nothing for it.
- **Delete and creation rollback** remove the Session's rows from every table, as today; the writer drops the
  Session's memo entries.

## 10. Compaction and checkpoint reads

A completed compaction changes the head of the list, so the next request shares no prefix with the previous one. It
starts a new list and adds a node for each of its messages (the replacement, the baseline and the retained tail),
bounded by the model's window. Following requests extend it. In the product-window profile each completed compaction
started exactly one new list (§4.2).

```text
 seq  event                      messages                         list
  30  input                      u18                              (resume of the snapshot below)
  37  model/call                 [u1 a1 ... u19]                  last node of u19, 37
  40  context/prepare            [u1 a1 ... u19 a19 u20]          2 new nodes, 39
  41  context/compaction-start
  42  context/compaction         completed: replacement S, retained from u18
  43  model/call                 [S b1 u18 a18 u19 a19 u20]       new list: 7 new nodes, at most the window
  50  context/prepare            [S b1 u18 ... u20 a20 u21]       2 new nodes, 9
```

The snapshot anchored at 42 records `resume` 30, the event of the first retained history entry. `resume` lies at or
before the compaction's `through` ([timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#压缩检查点),
`projection_checkpoint` in [history.rs](../../../crates/dim-agent-capabilities/src/history.rs)), so the request rows
between 30 and 42 were written before any snapshot chose it, and their lists reach back to `u1`.

A checkpoint read starts at 30 as today. Its first request row there is rebuilt by walking that row's own list: content
rows only, in holds of at most 256 node loads. No row that ends before 30 is read, and no request row before 30, which
is the `context_with_checkpoint` convention of
[timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描). The nodes it loads are the
messages of the requests the read returns, which v16 loads through those rows' references. The rows after it, the new
list at 43 included, continue from the kept request in order.

## 11. Format v17

**Decision: version 17, an in-place marker bump from exactly v16. No table, column or index changes, and no business
row is read or rewritten.** Recorded as proposed in [native-storage-format.md](native-storage-format.md#格式版本); the
full format decision moves there with the implementation.

- **Why a new version.** A v16 daemon cannot read a v17 request row: `References` rejects `list` and `length` and
  requires `messages`, and a v16 reader knows no `node` content. The format keeps a version only when old readers
  interpret the new data.
- **Schema.** The v16 tables as they are; `agent_store_format` becomes `CHECK (version = 17)`. `timeline_content.kind`
  has no SQL `CHECK` (the store checks it on read), so the kind `node` needs no schema change.
- **Mixed rows.** v16 rows stay as written and readable for good. After the upgrade a branch's next request row is v17
  and its previous request is a v16 row, which has no nodes, so that first v17 row writes a node for every message:
  one full list per branch, once, which later requests share. Rows of both versions mix in one branch and in one
  database, and a fork copies them as they are.
- **`session/forked`.** #222 added this event row at v16 without a version change, so a daemon built before it cannot
  restore a Session forked in a v16 store. v17 keeps the row as it is. Every such daemon refuses a v17 store, so a
  daemon that opens one decodes the row.

| Store found | Action at startup, in the existing admission transaction (`IMMEDIATE`, before writable PRAGMAs) |
| --- | --- |
| Empty | Create the v17 schema with marker 17 |
| Exact v17 schema, marker 17 | Open |
| Exact v16 schema, marker 16 | Recreate `agent_store_format` with `CHECK (version = 17)` and insert 17. Nothing else; a failure rolls back to the untouched v16 store |
| Exact v15 or v13 schema with its marker | The existing steps, then this one, in the same transaction |
| Anything else | Refuse and preserve, as today |

The upgrade is one-way, as S5a and S5b decided: a v16 daemon refuses an upgraded store with the existing "unsupported
SQLite schema … preserved" error. The daemon writes `Upgraded native store <path> from format v16 to v17; earlier
dimcode versions cannot open it.` to stderr once, after the upgrade commits. In `sqlite.rs` this is one entry,
`(16, &[])`, in `UPGRADES`, and `FORMAT = 17`.

No backfill: converting v16 rows would read and rewrite business rows, which an upgrade must not do, and #213 already
bounds their read cost.

## 12. Estimated effect

Projected storage, from the probe's exact v17 rows and content rows for each Session; every other row is unchanged.
Bytes are the `body` text of event rows plus `kind`, `hash` and `body` of content rows.

| Profile | Events | v16 request row | v17 request row | v16 per event | v17 per event | v16 total | v17 total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 1,001 | 7,248 B | 622 B | 1,720 B | 412 B | 1.72 MB | 0.41 MB |
| plain | 3,001 | 20,650 B | 624 B | 4,398 B | 411 B | 13.2 MB | 1.23 MB |
| plain | 10,001 | 67,551 B | 625 B | 13,778 B | 411 B | 137.8 MB | 4.11 MB |
| tools | 1,009 | 8,052 B | 622 B | 2,097 B | 466 B | 2.12 MB | 0.47 MB |
| tools | 3,007 | 22,928 B | 624 B | 5,401 B | 465 B | 16.2 MB | 1.40 MB |
| tools | 10,009 | 75,055 B | 625 B | 16,985 B | 465 B | 170.0 MB | 4.66 MB |
| product window | 3,007 | 4,575 B | 967 B | 1,613 B | 1,039 B | 4.85 MB | 3.13 MB |
| product window | 10,017 | 4,707 B | 968 B | 1,638 B | 1,046 B | 16.4 MB | 10.47 MB |

Request rows are averages over all request rows; SQLite pages, keys and indexes are not counted. Without compaction, v16
bytes per event grow with the history and v17 stays flat. A v17 request row holds references only; its messages are the
node bytes counted in the per-event and total columns. With compaction about every 20 turns and 1 KB messages, v16 is
already bounded and v17 still stores about 36% less: a v16 row repeats a 67-byte reference for every message of the
request, and v17 stores each message once per list. The retained tail of a compaction is stored again as new nodes,
since their parents changed, where v16 finds the same content rows.

Reads. The probe also copies each Session into a second disposable database with every request row rewritten to its
v17 form and the nodes it adds, and decodes it with a prototype v17 reader (row parse, event parse, list walk against
the kept request) that returns events equal to the v16 store's read of the same Session. Same machine and run; v16 is
the store on `main`, which includes #213.

Full-history read of `main`, median of five:

| Profile | Events | v16 store read | v17 prototype read | Node loads | Message pointers built |
| --- | --- | --- | --- | --- | --- |
| plain | 1,001 | 4.2 ms | 3.2 ms | 199 | 20,000 |
| plain | 3,001 | 21.4 ms | 9.9 ms | 599 | 180,000 |
| plain | 10,001 | 153.9 ms | 48.0 ms | 1,999 | 2,000,000 |
| tools | 1,009 | 4.5 ms | 3.5 ms | 223 | 25,088 |
| tools | 3,007 | 24.6 ms | 11.0 ms | 667 | 223,112 |
| tools | 10,009 | 183.2 ms | 51.7 ms | 2,223 | 2,473,088 |
| product window | 3,007 | 16.2 ms | 14.6 ms | 836 | 31,759 |
| product window | 10,017 | 59.9 ms | 65.5 ms | 2,844 | 109,576 |

The v16 reads at 10k (154 ms plain, 183 ms tools) match #213's own measurements (146 and 184 ms). Without compaction
each node is loaded once per read, so loads grow with the messages, not with the references. In the product window
v17 reads about as fast as v16 (14.6 against 16.2 ms, 65.5 against 59.9 ms): both are bounded by the window, and the
read loads 2,844 nodes where v16 loads 1,959 content rows, because each compaction's retained tail has new nodes.

Other work shared the machine during the runs; v16 and v17 alternate within each repetition, so the comparison is
fair, but absolute times carry that noise.

**Estimate (not measured on an implementation).** The prototype parses rows more cheaply than the store, which also
validates each row and handles branches and output rows. The estimate therefore takes the store's own per-event cost
from §2.3 (a ≈ 2.9 µs), adds 3.85 ns per message pointer and the prototype walk's 7.2 µs per node load (§7.4), and
uses the prototype's measured read as the lower bound.

| At about 10,140 events | Before #213 | With #213 | v17, estimated |
| --- | --- | --- | --- |
| Full-history read, plain | 208 ms | 146 ms | 48 to 52 ms |
| Restore and first idle, plain | 2,866 ms | 1,779 ms | 0.60 to 0.65 s |
| Full-history read, tools | 257 ms | 184 ms | 52 to 55 ms |
| Restore and first idle, tools | 3,624 ms | 2,256 ms | 0.67 to 0.71 s |
| Request row encode before SQL, plain | 542 µs | 542 µs | 13 µs |

- A full read is about `2.9 µs × 10,140 + 3.85 ns × P + 7.2 µs × L`. Plain turns build P ≈ 2.06 M pointers and load
  L ≈ 2,000 nodes; tool turns about 2.5 M and 2,224.
- A restore does 11 full reads plus `validate_session`, which costs about one read (§2.1, §2.2), and #213 leaves about
  24 ms (plain) and 46 ms (tools) of other work. Restore is therefore about 12 reads plus that.
- A model step that reads its context history from the store pays the per-read figure.
- With compaction, as in the product window, v17 reads about as fast as v16 and stores about a third less.

Acceptance 7 measures these numbers on the implementation.

## 13. Impact surface

Following [principles §3](../../architecture/rust-agent/principles.md#3-正交): the target is zero changes outside the
store and zero new public content.

| Module or content | Change | Reason |
| --- | --- | --- |
| `dim-app-server` storage (`codec.rs`, `timeline.rs`, `sqlite.rs`) | Encoding and decoding of lists, the writer memo, list walks in bounded connection holds, the fork's node closure, `FORMAT = 17` and one upgrade step | The format is storage's own concern (timeline-storage §2) |
| `TimelineStore` trait, Timeline, Dispatcher, event types, fixtures, capabilities, protocols | None | §3 |
| Public content added | None | The persisted JSON is internal to the store and versioned by its marker; no event, service, method, wire field or configuration |

## 14. Rule check

| Rule | Answer |
| --- | --- |
| [Principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 1 (command events for others' state) | Not applicable: no state of another module changes |
| §4 rule 2 (handover) | Not applicable: nothing is handed over |
| §4 rule 3 (before callbacks) | None added or changed. Callbacks still receive and return complete drafts; the store encodes the request drafts of a trace against their own record's request (§5.1) when it writes the record, after the before chain has run |
| §4 rule 4 (no business meaning in core) | The store encodes `ModelRequest` structurally (content-addressed messages and prefix lengths); it interprets no message and no business rule. Which events carry a request was already the store's knowledge in v16 |
| §4 rule 5 (atomic batches) | Unchanged. A row's nodes are written in the transaction that writes the row, or in an earlier one, so a committed row never references an uncommitted node; subscribers see the same events |
| §4 rule 6 (model context) | Unchanged: reads return the same events, so context is the same |
| §4 rule 7 (public events and services) | The store reads no plugin's state; the writer memo is the store's own |
| §4 rule 8 (infrastructure unchanged) | The Timeline, Dispatcher, event envelope and format, assembly and facades are unchanged. The requirement comes from the store itself (its read and write cost), and the change and its verification stay inside the store |
| §4 rule 9 (only decision facts in the Timeline) | No new event or fact; the encoding is not a Timeline fact |
| §1 (Capability and App Server boundary) | Inside the existing SessionStore plugin's `TimelineStore` implementation; no new plugin |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | Process scope: the store and its writer thread |
| ADR 002 D2 (no wiring modules) | No new module or plugin |
| ADR 002 D3 (dependencies follow scope) | No new dependency |
| ADR 002 D4 (configuration) | None |
| ADR 002 D5 (language-neutral contracts) | No contract changes; §5.2 defines the stored JSON as data |

## 15. Out of scope, and requirements to mechanism owners

None raised. Out of scope, as separate work:

- **Phase 3: restore-scoped shared read.** During a Session restore every capability reads `local(..=S)` for the same
  `S`. A read shared by the restore, decoded once and released when the Session publishes, would remove the remaining
  multiplier without changing a plugin. It is the runtime owner's decision and will be proposed with numbers once v17
  has landed. Designed and implemented as a read shared by each branch's opening:
  [restore-scoped shared history read](restore-shared-read.md).
- Converting existing v16 rows (§11).
- Event-type changes of any kind (§17).

## 16. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | Messages are content-addressed list nodes in `timeline_content`; a request row references its last node and the length | Deltas between request rows (this page's first draft); one reference per message (v16) | A delta chain runs through earlier request rows, so a checkpoint read would read rows before `resume`, against timeline-storage §4, and its cost followed the history rather than the request (§7.4). v16 grows with the square. A list keeps v16's split, so a read touches only its own rows and the content of the requests it returns |
| D2 | The writer shares the prefix with the immediately previous request in the logical history, compared by pointer and then `==` | The earlier request with the longest prefix; any request the writer still holds; byte-exact comparison | One deterministic rule; a search makes the bytes depend on the writer's state and costs a scan per write. Byte-exact comparison would serialize every `context/prepare` row's prefix, the v16 write cost; value equality is the events' own round-trip contract |
| D3 | `tools` and `system` stay v16 content references, reused from the previous request when equal | Inline in the row; "same as the previous row" | Content rows already address them; reuse spares the hash; a reference to another row would bring back reads of earlier rows |
| D4 | Drafts are encoded as lists against their record's request | Drafts with v16 references | The record's request is in memory on both sides, and a draft shares its nodes |
| D5 | A fixed-size writer memo per branch: position, `Weak`, list address and length, `tools` and `system` addresses; dropped on a failed transaction and pruned when dead | A strong memo; no memo; a memo with every node address | A strong memo would hold the last request of every Session the process ever wrote; without a memo every write reads its previous request back (§6.2); node addresses would make the entry grow with the request |
| D6 | No depth bound and no periodic full copy | A full list every N requests | A list holds exactly its request's messages, so a cold rebuild is one load per message of the result (§7.4); a periodic copy would only duplicate content |
| D7 | Nodes are content-addressed rows of the existing table | Integer-addressed nodes in a new table | No SQL change; v16's addressing, hash check and fork closure apply as they are. Integer addresses in creation order would walk with sequential locality (the first draft's row walk took 2.8 µs a step at 20,000, a node load 5.0 µs), but need a table, an id allocator and their own integrity check, for a gain that appears only beyond any measured profile |
| D8 | A list walk releases the connection after at most 256 node loads | One query or one hold per walk | timeline-storage §4 bounded segments; one recursive query held the connection for 598 ms at 20,000 messages (§7.4) |
| D9 | v16 rows stay as written | Converting them at upgrade or lazily | An upgrade reads or rewrites no business row (§11) |
| D10 | The writer reads any previous request it needs before its write transaction, through the read path | Rebuilding it inside the transaction | The transaction then holds the connection only for the new nodes and the row; a rebuild inside it would hold the connection for a whole list after every restart (§6.1) |

## 17. Rejected alternatives

- **Deltas between request rows** (this page's first draft: a row stores its base row's position, the kept length and
  the remaining messages inline). Rebuilding a request read every earlier request row back to the last compaction,
  so a checkpoint read from `resume` read request rows before `resume` (timeline-storage §4), and one recursive query
  held the connection for the whole chain. Decoded oldest first it was quadratic for one request; decoded backwards it
  was linear in the chain, but the chain follows the history: rows that add nothing and messages later rewritten all
  count (§7.4).
- **Deriving the request instead of storing it**: `model/call` is the final request handed to the model (ADR 005 D1),
  and ContextPolicy reads the exact latest request; a derivation is a second producer of the same fact.
- **Lazy request handles in events**: changes the event type and every consumer, and moves storage errors into
  readers that expect validated data (AGENTS.md data conventions).
- **Kind-filtered reads** (skipping request events for capabilities that ignore them): changes the read API and each
  capability's rebuild, and still leaves the quadratic storage.
- **Capability snapshots** for rebuild: business state for an optimization (timeline-storage §4) in every plugin.
- **A store-wide decoded-event cache**: residency would grow with every loaded or recently read Session, against the
  idle-branch acceptance.
- **Changing `ModelRequest.messages` to a persistent type**: changes the event type and its frozen contract for the
  pointer-copy term only (§7.4).

## 18. Acceptance

The implementation PR passes all of these, on disposable databases only. Behaviour checks run in the storage flows;
timing and size thresholds are measured with `restore_cost` and reported in the implementation PR, not asserted in
tests. The flows are the existing ones (`storage_flow.rs`, and `timeline_acceptance` for interruption and power loss),
not a test per bullet.

Carried from [timeline-storage §7](../../architecture/rust-agent/timeline-storage.md#7-验收):

1. **Storage round trip.** Events read back equal the written ones field by field, covering output rows appended
   across write rounds, atomic batches and before traces, plus: a new list after a compaction; a request that keeps
   part of the previous one and one that keeps all of it; `system` and `tools` absent, reused and changed; request
   drafts unchanged, with an appended message, with an early message changed, and with a changed system prompt; lists
   shared from a parent and from a grandparent, with the parent writing past the cut; mixed rows (a v16 Session
   continued after the upgrade, its first v17 row writing a full list); and a fork at a cut of a mixed `main`, whose
   reads equal the source's prefix and whose content rows are exactly the closure of its rows' references.
2. **Long-session residency.** 2,000 steps with an 800 KB context: an idle branch's live bytes do not grow with steps,
   and the peak during preparation does not grow with history before the compaction. After a Session unloads, the
   writer memo holds no live request of it.
3. **Paging and restore.** Long page reads do not decode the whole history first; a page that starts mid-history walks
   only its first request's list; merged rows, batches and cross-page tool calls stay correct. Restore caches are
   bounded (one kept request with its node addresses per read, and #213's cache), and a long cold read does not hold
   the connection so as to block writes.
   - **3a. Checkpoint read with a long history before `resume`, under a concurrent writer.** A Session with 2,000
     request rows before a compaction whose snapshot's `resume` lies a few turns before it. After the snapshot is
     saved, every event row that ends before `resume` has its body overwritten with invalid JSON in the disposable
     database; `HistoryRenderer::context`, the context-only read through the checkpoint, still returns the same
     `ContextHistory` as before the overwrite, so no such row was read. The store's read from `resume` loads exactly
     the first request's node count plus the new nodes of later requests. While that read runs cold, a writer
     appending to another Session never waits longer than one hold of 256 node loads.
   - **3b. One request.** Lists of 2,000 and 20,000 messages, append-only and with the last four messages rewritten
     each turn: a cold read of the last request loads exactly as many nodes as it has messages, holds the connection
     for at most 256 node loads at a time, and takes at most twice the v16 read of a request with the same
     messages on the same machine (§7.4 measured 0.7 to 1.65 times). After a restart, the first request written on such
     a branch reads its previous request in the same holds, and its write transaction loads no node.
4. **Process interruption and power loss.** Killed before commit, after commit and before publish, and after publish:
   the recovered events read back, sequences are contiguous, and no row references an uncommitted node. Writes after
   the restart (cold memo) produce the same bytes as an uninterrupted run.
5. **Storage failure.** Injected read and commit failures behave as today; after a failed commit the retried and later
   batches encode against committed rows and nodes only, with the bytes of a run without the failure.
6. **Corruption.** A missing node, a node whose hash does not match, a node of another kind or with an unparseable
   body, a list longer or shorter than its `length`, `list: null` with a nonzero `length`, malformed `ListReferences`,
   and `tools` or `system` addresses of the wrong kind: each fails `read`, `read_page` and `branches` with a storage
   error; the row keeps its bytes, and no request reads back empty. The same damage in the middle of a 2,000-message
   list fails every read whose requests pass through it, while requests in a later compaction's list still read.
   #213's content corruption cases still fail.

New:

7. **Many model calls between compactions.** Plain and tool Sessions grown to about 1k, 3k and 10k events without
   compaction (`restore_cost`). Report restore with first idle, per-read time and bytes per event at each size next to
   #213's numbers. Bytes per event at 10k stay within 10% of 1k, and at 10k plain events the per-read time is at most
   40% of #213's and restore with first idle at most half (1,779 ms on the machine used here), measured on the same
   machine. In the product-window profile per-read time stays within 1.2 times v16's and bytes per event below v16's.
8. **Determinism.** The same history, including a child branch, a compaction and request drafts, written once with
   the writer memo and once with it disabled (a `testing` hook), and once with a restart in the middle, gives identical
   `timeline_events` and `timeline_content` rows byte for byte.
9. **Frozen contracts.** `crates/dim-agent-types/tests/fixtures/` and the `*_contract.rs` tests are unchanged by the
   diff and pass.
10. **Upgrade.** An empty store is created at v17. A disposable v16 store upgrades in place to the exact v17 schema with
    every row unchanged byte for byte and reports it once; v15 and v13 stores reach v17 in one transaction; wrong
    markers, partial upgrades, older and unknown schemas are refused and preserved; the v16 daemon's refusal of a v17
    store is the existing error.

## 19. Changes to other documents

With this design: the [documentation index](../../INDEX.md) links this page, and
[native-storage-format.md](native-storage-format.md#格式版本) names v17 as the proposed next version.

With the implementation, each keeping its meaning: [native-storage-format.md](native-storage-format.md) (current
version and marker, the `node` kind of `timeline_content`, the persisted JSON types, read validation, the v17 decision
and the record example); [timeline-storage](../../architecture/rust-agent/timeline-storage.md) §2, whose "请求内容" row
will say that messages are stored as content-addressed list nodes and that the event body keeps the last node's
address and the length; [S5c §6.3](session-deletion-and-fork.md#63-what-a-fork-copies), whose `timeline_content` row
will add the nodes a copied list reaches; [ADR 001](../../adr/001-harness-foundation.md) (current format); and this
page's status. The bounded-read, checkpoint and acceptance conventions of timeline-storage §4 and §7 stay as written.

## 20. Open questions for reviewers

1. **Phase 3 owner.** A restore-scoped shared read (§15) belongs to the runtime. Who owns it, and should it wait for
   v17's measured numbers? Proposed after v17 with its numbers and reviewed by the runtime mechanism owner:
   [restore-scoped shared history read](restore-shared-read.md).
2. **One-way upgrade without a backup.** S5a and S5b decided this during development, and v17 follows them. Does that
   still hold for the release that ships v17, or should the upgrade keep a copy of the v16 database?
