# Timeline query selection

Status: Ready. Delivery status is tracked in #1147, not here.

Scope: process.
Issue: #1147.
Rules: [design rules](../architecture/rust-agent/principles.md), [ADR 002](../adr/002-plugin-scope.md), [ADR 019](../adr/019-event-query.md), [ADR 020](../adr/020-timeline-read-convergence.md).

## 1. Scope and non-goals

This document owns the selection fields of `TimelineQuery`: how a caller names the facts a read returns. It does not own business selectors (open pair, latest per key, a turn window), the context projection, the recall narrative, or the store's schema lifecycle beyond the derived search index.

The existing fields stay as they are: `scope`, `from_event`, `from_sequence`, `order_by`, `direction`, `include_publishing`, `kinds`, `flags`, `event_id`, `limit`, `offset`. This document adds predicates and text search; it removes nothing.

Non-goals:

- No general SQL expression language: no joins, aggregation, OR trees, nested arrays, or arbitrary functions.
- No business meaning in the store: turn, thing, recall body and open/closed state are the caller's.
- `open pair` and `latest per key` stay caller plans (two-step point reads and the keyed-block projection in #958), not query fields.
- Text search returns candidates; it is not the recall answer.
- No FTS index in this change: the text selection is a whole-data candidate scan. FTS5 is future work behind the same field.

## 2. Own contract

### 2.1 Predicates

A request may carry `predicate` (the existing single equality) or `predicates` (a new list), not both. A single predicate is the one-element list. The list is ANDed.

```rust
pub struct TimelineQueryPredicate {
    pub path: String,
    #[serde(default)]
    pub op: PredicateOp,
    pub value: serde_json::Value,
}

pub enum PredicateOp {
    Eq,        // default; the existing behaviour
    In,        // value is an array of scalars
    Prefix,    // value is a string; JSON text starts with it
    Contains,  // value is a string; JSON text contains it
    Exists,    // value is null; the path exists
}
```

- `Eq`: `json_extract(...) = ?`.
- `In`: `json_extract(...) IN (?, ?, ...)`.
- `Prefix`: `instr(json_extract(...), ?) = 1`; no pattern wildcards.
- `Contains`: `instr(json_extract(...), ?) > 0`; only for small selections, because it cannot use an index.
- `Exists`: `json_type(json_extract(...)) IS NOT NULL`.

`path` is one or more field names joined by dots, with at most one array segment:

```
a.b          // object path
blocks[].key // one array level; EXISTS over json_each('$.blocks')
```

An array segment expands to `EXISTS (SELECT 1 FROM json_each(...) e WHERE json_extract(e.value, '$.key') = ?)`. For a merged output row, the predicate applies to each `$.records[*].event.data` as well as the row's own data.

One array level is the maximum. A wildcard, a second `[]`, a leading `$`, or a bracket anywhere else is invalid.

### 2.2 Text search

`TimelineQuery` gains:

```rust
pub text: Option<TextSearch>,

pub struct TextSearch {
    pub terms: Vec<String>,
    #[serde(default)]
    pub mode: TextMatch, // All (default) | Any
}
```

- Both stores compare the serialized `event.data` value with `instr`, case-insensitively for ASCII; non-ASCII characters compare exactly. This is a candidate scan over the requested range, not an index and not a rank.
- The result is the matching events in the query's normal order and range; a text match adds no rank and does not reorder. The caller folds the candidates.
- `All` requires every term; `Any` requires one. Terms are matched as text, not as a query language.
- The scan may miss a term split across two events (for example two `step/text` deltas) and may match a field name or identifier that is not rendered. Recall verifies the rendered body, which removes false positives; it cannot restore a missed candidate. That narrowing is accepted for the first implementation.
- An FTS5 index (`trigram` for CJK, `unicode61` for Latin) is future work behind the same `text` field. It needs its own format decision and schema version; no request depends on an index today.
- Recall is the first consumer: it uses the candidates to read the surrounding turn and verifies every `about` word against the rendered body. The text result is a candidate set, never the answer.

### 2.3 Store pushdown

- `Eq`/`In`/`Prefix`/`Exists` are pushed to the store; the caller-side `accepts` still checks every predicate.
- A predicate with `Contains` or an array path may be applied in the store when the backend can express it; otherwise the store returns the range and the caller filters. It must not widen the range silently.
- `text` is pushed as an `instr` over `event.data` in SQLite and as the same comparison over the serialized data in the in-memory store. There is no index and no `InvalidInput` fallback in this implementation.
- `validate` rejects an unsupported operator, an invalid array path, a non-scalar `Eq`/`In` member, empty text terms, text with a named event, or both `predicate` and `predicates`.

### 2.4 Compatibility

`predicate` stays for existing requests. `predicates` and `text` are serde-default fields, so an old request deserializes unchanged. The generated `npm/branch-services.d.ts` is regenerated. A request carrying both `predicate` and `predicates` is invalid; a request carrying both `text` and `event_id` is invalid (a point read needs no search).

## 3. Events and state

The query is read-only. There is no text index in this implementation; the scan reads committed event data and no event shape changes. A later FTS index would be derived state, not a fact and not a source of truth; a missing or stale entry must only make a text search miss, never change a non-text read, and Session deletion removes its entries.

## 4. Dependencies

- `TimelineStore` implements the pushed predicates and the text scan. The port's existing range/order contract is unchanged.
- SQLite and the in-memory store both implement the same selection; a backend that cannot express an operator fails validation rather than returning a different set.
- Recall consumes the text candidates; its narrative and verification stay in the capability.

## 5. Cross-module requirements

None. The change is inside the Timeline query contract and the stores that implement it; no plugin needs a new port or a special case.

## 6. Rule check

- §4 rule 1 (placement): no new part; the query is infrastructure.
- §4 rule 2 (facts): the read is selection only; no new fact.
- §4 rule 4 (state): no business state; the text index is derived.
- §4 rule 9 (publish after commit): unchanged.
- ADR 002 D1–D5: no plugin-specific handling; the contract is language-neutral; the Rust and TypeScript shapes are bindings.
- ADR 019/020: one bounded read; the additions do not make the store decide business bounds.

## 7. Binding appendix

The Rust shapes are in §2.1 and §2.2. The TypeScript declaration is generated from them with `cargo run -p xtask -- types`; `PredicateOp` and `TextMatch` use their serde spellings (`eq`, `in`, `prefix`, `contains`, `exists`; `all`, `any`).
