# 019: One bounded event query is the Timeline read surface

Decision status: Proposed.

## Context

Capabilities read Timeline facts through the branch's Timeline service. That service currently
carries one read per consumer shape: whole-chain history, a local range, a page after a cursor,
a context page, the branch segments, one kind across the ancestors, one event by id and by
position, the folded context with its checkpoint, and the policy order, window and facts.

Each method encodes what one consumer wants. The runtime therefore defines what a context page,
a policy window or a recall page is, and other languages see an arbitrary subset of those
methods. Several of the reads are unbounded: the fold that decides a fork rest point, the recall
tool's history read, and the replay a part registers when a branch opens, which reads the branch
from its first event to the published head and shares one decode while the branch is opening.

The event envelope and its data are a frozen compatibility contract;
`crates/dim-agent-types/tests/fixtures/` is the baseline every compatible store must decode, and
`crates/dim-agent-types/tests/wire_contract.rs` pins that wire shape. The persisted layout is a
different contract: it has its own format version, schema, indexes and migration, and it is a
contract between stores, not an interface for a capability.

## Decision

### D1. One bounded query over events

The Timeline exposes one read for capabilities: the scope (this branch, or the branch chain —
each ancestor up to its cut, root first, then this branch to its head), the accepted event kinds
plus the envelope's two
context flags, one event id, one equality predicate over a data field, where the read begins
(an event id or a sequence), the order and its direction, and the page: how many matching events
it skips and how many it returns, the second nonzero by type. It returns
decoded events, each with its chain position.

A kind or field path the store does not know matches no rows; it is not an error. A capability
that needs a different selection extends this query; it does not add a method.

### D2. Fixed semantics

The elements combine: an event is returned when it satisfies all of them, flags named together
are alternatives, and an absent selection constrains nothing. Results are ordered by chain
position, then sequence, in the requested direction.

Paging is skipping, not a cursor: the caller repeats the query with a larger skip, and a page
shorter than the count is what says the selection is exhausted. A page therefore does not align
with an atomic batch — it may end inside one, and the next page carries the rest, which a
consumer folding in order handles. A read is taken at what is published when it runs, so a consumer
that folds several pages drops what it has already folded.

The existing reads do not share this convention: the context page accepts a cursor inside a batch
and returns the row it names, and the history read has no page. A consumer that still uses its own
read keeps that read's behavior until it migrates, and the migration states which convention it
adopts.

### D3. The query addresses the event contract, not the storage

Addressable: the session, the branch, the sequence (only as where a read begins), the kind, the event id, the two context
flags, and paths inside the event's data. Not addressable: table names, column names, index
names, the content table, batch metadata, and any other storage layout. SQL text is not a
capability interface.

### D4. Indexes are derived or part of the format, never named by a capability

An index that serves the storage format stays part of the format and its gate. An index added
for a query is a derived object created at open: it carries no data, is recreated on every open,
and is outside the format gate. A capability never names either.

### D5. Consumer-shaped reads are temporary

A read shaped for one consumer exists only while that consumer moves to the query. The service
keeps three things: append one event, read the published sequence, and this query. Branch
segments and recovery snapshots are not events — the first is the branch directory, the second
is derived state — and keep typed reads of their own.

## Consequences

- A new capability read adds no service method, and the runtime stops defining context pages,
  policy windows and recall pages.
- A read names a selection, not a slice of the store, so a store executes the selection where the
  events are kept and a read costs what its page covers. A consumer that folds a whole prefix
  folds it from pages; an unbounded read has no interface.
- Rust and other languages see the same query; the generated declarations describe it once.
- A selection the query cannot express is a decision to extend the query here, not a method to
  add. No consumer keeps a private read past its migration.
- While a consumer still uses its old read, both shapes exist and must agree on ordering and on
  where a page ends — or the difference is stated where the migration is tracked.

## Non-goals

- Changing event types, the envelope, the storage format or any fixture.
- Deciding where the context projection lives.
- A general expression language: the first form is equality on one data field.
- Folding, projecting or aggregating in the query; it returns events.
- Covering branch segments and recovery snapshots.

## Supersession

Follows [ADR 006](006-plugin-contract.md) D1: a rule is stated once, at its layer. Supersedes no
ADR.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | No new part. The query is a Timeline service method the branch's parts already hold. |
| §4 rule 1 | Reads do not change assembly, dependencies or activation. |
| §4 rule 2 | Consumers keep deciding from facts; the query selects facts, it does not decide. |
| §4 rule 3 | No `before` callback and no commit-time I/O. |
| §4 rule 4 | State stays with its owner: a fold remains in the part that owns the state. |
| §4 rule 5 | Unchanged: appends and their batch contract. |
| §4 rule 6 | Unchanged: context enters only through adopted inputs and tool results. |
| §4 rule 7 | Callers keep reading through the Session and branch facades; the query is a public service and speaks only public event data. |
| §4 rule 8 | Requirement raised to the Timeline's owner: one bounded query, its paging (skip and count) and ordering semantics, and the derived-index rule. |
| §4 rule 9 | No new fact. |
| ADR 002 D2, D3, D5 | No wiring part, no dependency crossing, and the query is language-neutral data. |

## Requirements raised to mechanism owners

The Timeline service owner (runtime) provides the query, its scope, paging (skip and count) and ordering
semantics, and the derived-index rule. The storage owners (SQLite and the in-memory store)
implement the same semantics. The plugin contract describes the query as a public service, and
the owner of each consumer-shaped read replaces it with the query.
