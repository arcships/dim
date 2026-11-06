# Keyed blocks: the `session.keys` read

Status: Ready. Delivery status is tracked in the issue, not here ([#958](https://github.com/arcships/dim/issues/958)).
Scope: the protocol v3 `session.keys` read over a branch's own events. It adds no event, service, part or persistent state; it is served by the built-in Rpc protocol like `session.history`.

## 1. Purpose and boundary

Keyed raw input blocks are core block data. A raw block carries an optional key ([session access A.1](../contracts/session-access.md#a1-the-sessions-service) `RawBlock.key {use: live | retire, name}`); keys live per branch Timeline, each writer chooses its own key, and nothing checks it (A.2). Producers write them through the ordinary input path: Goal writes `goal:objective` and `goal:status` ([Goal](../plugins/Goal.md) §2, A2), TimelineTree writes `branch:line` and `branch:parent`.

A client that shows live keyed state folds it from the branch history it loads and the live `session.event` notifications it receives. A bounded history window (the newest turn, [session-history-window](session-history-window.md)) can start after the input that set a key, so a client that reopens a Session from a window can lose live state it never loaded. The same fold exists once per client, and each one has to rediscover the very same rules (opening reset, retire, deleted input).

`session.keys` projects the branch's own segment at a fixed position to exactly its live keyed blocks. It is generic: it does not know Goal, does not interpret a block's text, and adds no Goal semantics. The Goal branch plugin keeps folding its own blocks server-side (A3, A14); the read serves clients.

The boundary against Goal's decisions: Goal A8 refuses a Goal-specific status query because it would need a Session part to read a per-branch service ([ADR 002](../../adr/002-plugin-scope.md) D3). This read is not that: it returns core block data no part owns, is served by the Rpc built-in protocol next to `session.history`, and leaves A8's decision and its reason intact. Goal §6 and A8 name it as the path a client may read the goal from when the App Server serves it.

The read adds no event, service, plugin, part or persisted state. It reads through the one event query ([ADR 019](../../adr/019-event-query.md)), through the loaded line's Session or, once the line has ended, through TimelineTree at its fixed head ([ADR 021](../../adr/021-reads-follow-the-state-owner.md) D2, D3).

## 2. Contract

The shapes follow [session access A.1](../contracts/session-access.md#a1-the-sessions-service).

### 2.1 Request

```text
// KeysRequest
{ "sessionId": "string",
  "branchId": "string",              // required on a v3 connection, as for every v3 session
                                     // method that names a branch; clients send "main" for the root line
  "through?": 0 }                    // fixed read position; absent: the published head (session.sequence)
```

### 2.2 Reply

```text
// KeysResult
{ "through": 0,                      // where the projection is fixed: the request's through, or the
                                     // published head the read took
  "entries": [ KeyEntry ] }          // exactly one per live key of the branch's own segment; an
                                     // empty array, never null
// KeyEntry
{ "key": "string",                   // RawBlock.key.name, for example "goal:objective"
  "sequence": 0,                     // sequence of the input/message event carrying the block
  "inputId": "string",               // that event's id, the same id space as input/deleted.inputId
  "text": "string" }                 // RawBlock.text, byte for byte
```

- `entries` holds exactly one item per live key of the branch's own segment: the newest live block of each key, ascending by the carrying input's `sequence`, ties inside one input by block order. The reply is deterministic for a fixed `through`.
- `text` is the raw block text: no escaping, no rendering, no marker rewriting. A client parses whatever block format it owns from exactly the bytes its server-side counterpart sees.
- A retire block produces no entry: it carries no information once the key has left effect. A block without a key produces none either.
- Empty branch, or every key retired: `{through, entries: []}`. Clients do not need to distinguish the two.
- A fork's `main` excludes everything below its `session/forked`; a child branch excludes everything below its `branch/start`. Copied and inherited facts are not the branch's own segment.

The request has no `keys` filter. The reply always lists every live key of the segment, unknown ones included; a client reads the keys it knows and ignores the rest — not an error, no re-read, no effect on other keys. A filter would not reduce the walk (a key last set a thousand messages ago still has to be found) and would add a second unknown-key rule on top of this one.

There is no paging. The entry count grows with the number of live keys, not with the branch's length. `key` is writer-chosen and unchecked, so the count is bounded here: more than 1024 live keys fails the call with the non-retryable application error `too_many_keys`. The read never truncates: truncation would silently lose live state. Goal contributes 2 keys and TimelineTree 2, so the bound is reached only by a producer outside the documented contracts. The reply is also bounded by size: its serialized JSON must fit the same reply-frame bound `session.history` measures against (`outbound::MAX_FRAME_BYTES` less 64 KiB of envelope), and a larger one fails `keys_too_large` instead of being dropped by the transport. A few large blocks reach that bound before the key count does. Truncation is not an answer to it either, for the same reason.

### 2.3 Rules

- A key is in effect while the newest live block of that key, on the branch's own segment at `through`, is a `live` block. A later block with the same key replaces it; a `retire` block ends it.
- A block of an input that `input/deleted` removes never takes effect, neither a `live` block nor a `retire` block. The key keeps the newest block that is still live, or is absent.
- The segment opens at the branch's first opening event: `branch/start` for a child line and `session/forked` for a fork's `main`. Nothing below it belongs to the segment.

### 2.4 Errors

| Error | When |
| --- | --- |
| `unknown_method` | The connection is below protocol v3, like `session.history`; `input/deleted` is not visible on a v2 connection, so the projection cannot be served there |
| `invalid_params` | `branchId` is missing or empty on a v3 connection; `through` is past the published head |
| `session_invalid_input` | The Session or the branch does not exist (the acquire and read outcomes of every session method) |
| `session_closed`, `branch_ended`, `service_reloading` | The Session closed, the line ended before the read could run, or the serving service reloads; as `session.history` |
| `too_many_keys` | More than 1024 live keys; not retryable, a retry answers the same |
| `keys_too_large` | The serialized reply exceeds the reply-frame bound; not retryable, a retry answers the same |

A `through` past the published head is invalid input, never clamped to the head: a client asks for a position it observed, and one beyond this connection's head means it built its overlay from a different view of the branch. Clamping would answer a position the client did not ask for.

## 3. Read algorithm and cost

One descending scan, `TimelineQuery` with `kinds: ["input/message", "input/deleted", "branch/start", "session/forked"]`, `TimelineQueryDirection::Descending`, ordered by sequence, `limit` 256 per page. The scan starts at `through` and stops at the first opening event; everything below it is discarded. Pages advance by `from_sequence = min_sequence - 1` of the previous page, never by `offset`: the query's offset is skipping, and the storage reader fetches `offset + limit` rows before skipping them, so a deep walk by offset is quadratic in the page count. A page shorter than the limit means the selection is exhausted (ADR 019 D2). Cost is O(input/message and input/deleted rows above the opening), one zone query per page over `(session_id, branch_id, sequence)`; a 20,000-message branch is about 80 pages.

Deletions are collected in the same pass, without a second read. The ordering that makes it correct: InputQueue writes `input/deleted` only for an input it still holds unapplied — a withdrawal is decided when the input has neither `input/applied` nor a consumption — so the deletion event is always written after the input it removes. A descending scan therefore meets the deletion before it meets the block, and a block is live only when its input id is absent from the set collected so far. A `retire` block written by a deleted input is skipped by the same check, so it cannot end a key.

Each event's keyed blocks are read in reverse block order (newest block of the input first), so the newest block of a key wins within one input as well as across inputs. A key already decided by a newer event is skipped; a `retire` of a key that has no newer live block records the key as closed for every older block. The cap is enforced while collecting, so an over-cap reply fails before the walk ends.

No new storage capability is needed: kinds are pushed into the storage selection, and descending, range and limit already exist. What does not exist is a block-level key predicate — the query's predicate is one JSON scalar point path and rejects arrays — so the scan reads every input message above the opening, not only the keyed ones. A derived index on raw-block keys would be a derived object under ADR 019 D4 and is out of scope here.

The read follows the two-owner pattern of `session.history`: an open line is read through its Session (`Session::query`, which reads at the published head), an ended line through TimelineTree at its fixed head (`TreeCommands::query`). A line that ends while the call runs answers `branch_ended`; the read is then repeated through the tree at the fixed head. Unlike the `latestTurn` window, the read needs no native store: both owners serve it.

## 4. `through` and the client's live overlay

A client uses `reply.through` as its boundary. Blocks written at or below it are exactly those the reply reflects; the client applies the events above it that it already receives:

1. For an `input/message` at `sequence > through`: apply its raw blocks in order. A live block sets that key's current block to `{key, sequence, inputId, text}`; a retire block removes it.
2. For an `input/deleted`: for each key whose retained blocks include that `inputId`, remove the deleted input's blocks and take the newest remaining block of that key as current; if none remains, the key is absent. A client therefore has to retain every keyed block it has seen since `through`, not only the newest block per key: an `input/deleted` cannot fall back to an earlier live block that the client already discarded.

The guarantee this rests on: the projection is a pure function of the segment's events after the opening — blocks, retire and deleted inputs only. Compaction writes `context/*` events, deletes no event and carries no key; events are append-only. Therefore

```text
reply(through) ⊕ overlay of (through, through'] = reply(through')
```

for one connection's view. A client that wants to check its overlay can re-read at any later position and compare.

A client that opens a Session reads at the head, or pins `through` to the same position it uses for the history it loads, so both views describe the same point of the branch.

## 5. Equivalence with the model baseline

The model's baseline of keyed blocks advances at `input/consumed`: `history.rs` replays the prefix, renders each consumed input's blocks (`render_input`) and applies them to `delivered_state`, and a compaction record copies that baseline ([history.rs](../../../crates/dim-agent-capabilities/src/history.rs)). The projection here advances at `input/message`, the point where Goal A14 makes a change take effect.

The exact relation is:

```text
projection(through) = delivered_state(through)
                    ⊕ keyed blocks of inputs written at or below through
                      that are neither consumed nor deleted at through
```

The two sides agree exactly at a quiescent point: a `through` where no keyed input is written but unconsumed, for example the end of a settled turn or a compaction cut. At such a point every entry satisfies, per key:

- `entry.key` equals the baseline entry's `key`;
- the baseline entry's `message` is one text part, and its bytes equal `entry.text` — `render_input` builds `Message::text(Role::User, text)` from the same `RawBlock.text`, with no escaping or rendering on either side;
- the two ids name the same input fact: `entry.inputId` is the `input/message` event that carried the block, and `ContextBaselineEntry.source_event` is the `input/consumed` event that placed it in the baseline (`render_input` receives the consumed event), one event apart in the same branch.

The two orders differ by construction and are not compared: the baseline lists a key at its first appearance, the projection orders by sequence. A comparison is a key-to-entry map.

## 6. Rule check

| Rule | How this read satisfies it |
| --- | --- |
| [ADR 002](../../adr/002-plugin-scope.md) D2 (no wiring module; a service that needs a home is a part or a built-in) | Rpc dispatches `session.keys` as a built-in next to `session.history`; no part is created and no `sessionMethods` namespace is registered |
| ADR 002 D3 (assembly makes no decisions from branch state) | The read is a client protocol method; it adds no part, no load order and no config-dependent assembly |
| [ADR 019](../../adr/019-event-query.md) D1 (one bounded query over events) | The scan is a `TimelineQuery`; no new read API is added |
| ADR 019 D2 (fixed semantics) | Descending pages ordered by sequence, a short page means exhausted, and the advance is a range, not an offset |
| ADR 019 D3 (the query addresses the event contract) | Only kinds and a sequence start are named; no table, column or index |
| ADR 019 D4 (indexes are derived or part of the format) | No index is added; the block-key predicate is not expressible in the query and stays out |
| [ADR 020](../../adr/020-timeline-read-convergence.md) D1 (one read, the facades forward it) | Reads go through `Session::query` and `TreeCommands::query`, the same facades `session.history` uses |
| [ADR 021](../../adr/021-reads-follow-the-state-owner.md) D3 (a client read requires a live Session) | The call acquires the Session; there is no branch that reads store facts for a Session the process does not hold |
| ADR 021 D2 (an ended line is read through the object that owns its end) | An ended line is read through TimelineTree at its fixed head, including after the line ends mid-call |

Goal A8 is unchanged: a Goal-specific query would need a Session part to read a per-branch service, which this read does not. It reads core block data that no part owns, and Goal §6 points clients at it as an alternative to folding the history.

## 7. Compatibility

- The method is protocol v3 only, like `session.history`: a v2 connection answers `unknown_method`, because a v2 page does not carry `input/deleted` and the projection would be wrong.
- The capability is advertised in `hello.features` as `session.keys` when Session execution is on, next to `session.history`; a client sends the call only when it is advertised.
- Older clients keep their history folds unchanged: the read adds no event, changes no existing reply, and both Goal §6 and A8 name the history fold as the path when the method is not served.
- The read is a projection of facts already on the wire, so a fork and a reload serve it from the same events; no state is persisted for it.

## 8. Acceptance

- A branch with two keys set in one input, then one retired, then one set again: the reply carries the newest live block of each key, ascending by sequence and by block order inside one input, with the right `inputId` and `text`.
- An `input/deleted` on the input that wrote a key: its blocks never take effect; a key falls back to an earlier live block when one exists and is absent otherwise; a retire written by a deleted input does not end the key.
- A fork's `main` and a child branch list none of the parent's keys, and a key written on the child or the fork appears only there.
- `through` before a later change returns the older value; a `through` past the head is `invalid_params`.
- A v2 connection answers `unknown_method`; an ended line is served at its fixed head with the entries the open line folded.
- A read fixed at position 0 answers `{through: 0, entries: []}`; a fully retired branch lists no entries; more than 1024 live keys is `too_many_keys`.
- Two keyed inputs whose texts together exceed the reply-frame bound answer `keys_too_large`, and the same connection still answers the next call; a reply just under the bound is served.
- At a quiescent point the reply equals the last `HistoryEntry.baseline` by key: same keys, same text bytes, and each id names the same input fact.

## 9. Changes to other documents

- [Goal](../plugins/Goal.md) §6's "Reading the goal" row and Appendix E A8 name this read as the client path when the App Server serves it.
- [Session access](../contracts/session-access.md) §1 lists the read among the contracts it defines, A.1 carries `KeysRequest`, `KeysResult` and `KeyEntry`, and the context mapping's "Deleted input" row states that a deleted input is absent from `session.keys`.
- [Documentation index](../../INDEX.md) lists this page.
