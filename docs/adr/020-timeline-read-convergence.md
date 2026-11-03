# 020: Timeline reads converge on the event query and replay is removed

Decision status: Proposed.

## Context

[ADR 019](019-event-query.md) makes one bounded query the Timeline read surface and says consumer-shaped reads are temporary (D5). It does not say how every existing read leaves, who still reads around the service, or how a part rebuilds once the reads are gone.

An audit of every read entry, with its call sites and what each consumer needs, is in [Timeline read entries](../design/timeline-read-entries.md). It shows that the same rows are reachable through the Timeline service, the `Branch` and `Session` facades, the Dispatcher exports, the TimelineTree part's own store reads and the App Server's `SessionStore` reads. It also shows that eleven parts rebuild from a replay of the whole branch prefix to the committed position, although most need only a few kinds, and two need only the last event.

## Decision

### D1. One read, and the facades only forward it

The Timeline service keeps append, the published sequence, the event query, the branch segments and the recovery snapshots. `Branch` and `Session` keep one `query` that forwards to the service and bounds the read at the published head. The Dispatcher has no reads. Every other read on these objects is removed when its last consumer has moved.

### D2. Selection and granularity

One equality is pushed down to the store. A further condition is applied by the caller; a sparse match costs extra pages, and a short page still means the selection is exhausted. The event stays the unit of the log: no `step_id` is added and `step/text` and `step/thinking` are unchanged. A merged storage row stays inside the store: offset, limit, start, bound and predicate apply to events after the row is expanded, and every row a statement selects must hold at least one event that passes the filter.

### D3. Replay is removed

The Dispatcher delivers live events only and no longer reads history. A part rebuilds from the query, limited to the kinds its rebuild folds, which are not always the kinds it subscribes to. A part that must also see facts written but not yet dispatched reads them through a Timeline read that adds the undispatched tail of the hot window; a part with no such need rebuilds from the query and its subscription. The handover is: subscribe and hold live events, take the cutoff, read history and tail up to it, then release the held events above it. It is checked against the subscription mechanism in the first migration, with a test that the old and new rebuild give the same part state.

### D4. Batch structure stays on the store port

Whether a sequence ends an atomic batch is storage structure, not event content. The check stays on the store port and is not a field of the query.

### D5. The checkpoint names its resume event

A recovery checkpoint stores the event id of its resume point beside `{branch_id, sequence}`, and a chain read continues from it with `from_event`, which names its branch. This changes a persisted structure, so it needs its own format decision with a version and a migration, recorded in the storage format design and tracked by its own issue. Until that lands the checkpoint keeps its position.

### D6. Reads that go around the service

Reads of loaded sessions that bypass the service (`Session::local_page` and the TimelineTree part's own store read) move to the query. For a session that is not loaded no Timeline service exists, so the App Server keeps one `SessionStore` page read with the page shape of the query; loading a session does not change the protocol.

## Consequences

- The reads listed in the audit are removed from `Timeline`, `Branch`, `Session` and the Dispatcher as their consumers move; `xtask types` and `xtask graph` regenerate the generated files and nothing else in them changes.
- Plugin contract §5.2 describes the replay a rebuilding part receives; it is rewritten with the first rebuild that stops using replay.
- The shared decoded copy of a branch during opening exists for replay readers and is reconsidered when the last of them moves.
- Consumers that read to the committed position (goal, file reads, the collaboration rebuild) decide in their own migration whether they need in-flight facts.

## Non-goals

- Changing the event envelope, its data or the frozen fixtures.
- Changing the query's request or reply; that is ADR 019 and the plugin contract.
- Folding, projecting or aggregating in the query.
- The persisted layout of merged rows.

## Supersession

Refines [ADR 019](019-event-query.md) D5 and follows [ADR 006](006-plugin-contract.md) D1. Supersedes no ADR.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | No new part. Each part rebuilds its own state from its own reads. |
| §4 rule 1 | Rebuild and live delivery start after publication, as today. |
| §4 rule 2 | Parts keep deciding from facts; the query selects facts. |
| §4 rule 3 | No cold read inside a handler; the rebuild runs in the part's own task. |
| §4 rule 4 | Business memory stays rebuildable from the Timeline; the rebuild is a query. |
| §4 rule 5 | Unchanged: appends and their batch contract. |
| §4 rule 7 | Callers read through the branch's Timeline service; the facades only forward. |
| §4 rule 8 | Requirements raised below. |
| §4 rule 9 | No new fact; D5 changes the optional recovery snapshot, not a required fact. |
| ADR 002 D2, D3, D5 | No wiring part; the query is language-neutral data. |

## Requirements raised to mechanism owners

The Dispatcher owner removes history reads and replay and keeps live delivery. The Timeline owner provides the read that adds the undispatched tail and keeps the batch-end check on the store port. The storage owners keep merged rows inside the store and decide the checkpoint format change. The owner of each consumer moves its reads and states what it folds.
