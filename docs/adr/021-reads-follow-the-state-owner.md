# 021: A read follows the object that owns the state

Decision status: Proposed.

## Context

[ADR 019](019-event-query.md) makes one bounded query the read surface of a Timeline, and [ADR 020](020-timeline-read-convergence.md) removes the consumer-shaped reads and replay. Both describe a line that is loaded: a `Timeline` exists, it owns append, the publication state and the query, and `Branch` and `Session` forward the query.

They do not say what reads when no `Timeline` exists, and three places read then, around the service:

- `session.history` for a Session the process does not hold (`dim-app-server/src/agents.rs`, `history_page`) falls to `SessionStore::read_page_without_request_bodies`, which calls `SqliteTimelineStore` directly.
- An ended line's page falls to the same page read when the host keeps a store, and to `TimelineTree`'s own `read_page` when it does not.
- `TimelineTree` reads `branch/ended`, `branch/end` and `branch/create` through `TimelineStore::read` and `read_page` while restoring lines.

The result is that the page rules — ascending order, the published bound, an end that falls on a batch boundary, and the request-body decode policy — exist twice: once in the `Timeline` query path, which adds `batch_end` in the caller, and once in the App Server's SQL. Nothing keeps the two equal.

The store port is also reachable from any part that declares it, so a part can read facts around the service it is supposed to use.

A stored Session is not the same as an unmounted one. Any peer a caller interacts with has a running Session, and therefore a `Timeline`; a stored line that ended has no `Timeline`, but its Session still does. "No live object" therefore describes a line, not a Session, in every case that needs to read.

## Decision

### D1. A loaded line is read only through its Timeline

The Timeline of a loaded line is the read surface for that line: the query, the checkpoint selection, the segments and the batch-end check. `Branch` and `Session` keep forwarding the query. No caller reads a loaded line's facts through the store port.

### D2. An ended line is read through the object that owns its end

A line that ended still belongs to a live Session, and `TimelineTree` owns its ended head, its end record and its creation source. A read of its history is a read of stored facts bounded by that head; the tree serves it. The client protocol reaches it through the tree's own service, and not through a page read of the store by the App Server.

### D3. A client read requires a live Session

`session.history` and the in-process `sessions.history` it backs both acquire the Session, which loads it when it is idle. The client protocol has no branch that reads store facts for a Session the process does not hold. Loading builds the runtime objects; the facts are still read in pages.

### D4. The only read outside a Session is the database export

`dimcode --export-timeline` reads a database file and writes it out. It mounts nothing, has no Session and no line, and says so. There is no general process-level history API: every other read either belongs to a live Session or is the ended-line read of D2.

### D5. The store port is reachable from the Timeline and from TimelineTree

`TimelineStore` is a runtime port. `Timeline` and `TimelineTree` may declare it; `Session`, `Branch` and other parts may not. `SessionStore` keeps Session files, titles, grants, fork and delete, and serves no event read for the client protocol.

### D6. A read fetches one storage page at a time

A read takes its facts from storage one page at a time: a page is consumed and dropped before the next is fetched, and its size belongs to the caller. This bounds what a read takes from the store, not what a consumer keeps. The query contract lets a consumer fold a prefix from pages (plugin contract §5.4), and an export materializes what it writes; both are accumulation by design, not a second read of the store.

Three readers accumulate beyond one page today and are named in the design document: the opening copy that kept `1..=head` in memory was built for replay and goes ([#1063](https://github.com/arcships/dim/issues/1063)); `dim export` collects every page of a Session, and `chain_through` keeps the prefix the fork check folds ([#1065](https://github.com/arcships/dim/issues/1065)).

## Consequences

- D3 and D5 are the target, not the current code: `agents.rs::history_page` still falls to `SessionStore` when a store exists (`history_page`, and `read_page` for a peer). The implementation removes that fallback, and a flow test reads the history of an idle Session and observes the acquire instead of a store read.
- `agents.rs::history_page` loses the branch that reads `SessionStore` for a Session the process does not hold. A Session that is not loaded is loaded by the acquire, and an ended line is served by `TimelineTree` whether or not the host keeps a store.
- `idle-unload.md` acceptance 1 and 2 describe `session.summaries` listing unloaded Sessions and their history reading in full without a restore. `summaries` reads metadata, not history; the history read acquires the Session, so the acceptance sentence is rewritten to match.
- The page rules have one owner per path: the query path for a live line and the tree's stored read for an ended line. A divergence between them is a defect, tested against the query's page shape.
- `TimelineTree` keeps the store port for the reads of lines it owns while they are not loaded; four of those reads select events and become the port's query, which changes cost, not ownership ([#1064](https://github.com/arcships/dim/issues/1064)).
- The App Server no longer calls methods of `SqliteTimelineStore` that the port does not declare. Where the port lacks a capability the tree needs, the port gains it.
- This supersedes ADR 020 D6's sentence that an unloaded Session keeps one `SessionStore` page read and that loading does not change the protocol. It also refines ADR 019 D1 for the one read that is not a Session read.

## Non-goals

This decision does not change the query's selection, the publication bound, the checkpoint format, the batch-end check or the rebuild handover. It does not add a stateful object for a line that is not loaded.

## Supersession

Supersedes [020](020-timeline-read-convergence.md) D6's unloaded-Session clause. Refines [019](019-event-query.md) D1 for the ended-line read.
