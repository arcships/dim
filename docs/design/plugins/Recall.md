# Recall

Scope: session. Issue: #2026.

Recall owns the `recall` tool: the model asks for its own line's history, and the tool pages the line's facts at a pinned head. The current implementation is in `crates/dim-agent-capabilities/src/recall.rs`; the plugin page is [Recall](../../architecture/rust-agent/plugins/Recall.md). The history read is TimelineTree's (#2024); this owner holds no state.

## Use and scope: responsibility split

Recall owns:

- **The tool's declaration and result text.** What a call returns: turn, input, reply and tool origins, ranked and paged.
- **The page budget.** At most 128 origin positions, 20 entries or 8,000 characters per page, eight bodies per batch.
- **The pinned cutoff of one call.** A compaction during the call cannot move it.

Callers keep:

- What the model wants: the tool's arguments are the model's.
- The line's facts: TimelineTree answers them at a cutoff; this owner only selects and renders.
- Context assembly: the result is one tool result.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model reads its line's history | No: the selection, ranking and paging are this owner's | the `recall` tool |
| The model names a cutoff or a page | Yes: the tool's arguments | Business |
| The tool pins one head for the call | No: the pin is TimelineTree's read and this owner's selection | TimelineTree's `read`, called once |

An operation or parameter exists only where the row says No.

## Caller example

The tool resolves the calling line and reads at its head; no `Branch` object is involved:

```rust
let line = execution.branch_id; // a value
let ReadLinesReply { through, events } = tree.read(ReadLines {
    line: line.clone(),
    scope: None,
    from: Some(after + 1),
    through: Some(head),
    fields: RECALL_FIELDS.to_vec(),
    limit: Some(PAGE),
}).await?;
render(events)
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:timelineTree` | `read` at the line's cutoff; the directory record names the head. |
| `S:toolRegistry` | Register `recall` until this part unloads. |

### Contributions

No service of its own; the `recall` tool is the whole surface.

### Errors

The tool answers `ToolResult`s; the tree's read errors become the tool's content: `unknown` (the line is not this Session's), `beyond_head` (lower the cutoff), `not_batch_boundary` (use a reply's `through`), `continuation_without_cutoff`, `closed` (the Session is closing). A cancel after the first page answers the tool as cancelled without further pages.

### Flows

| Path | Normal | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Recall | Resolve the line, read the index page, load at most eight bodies, render, page | The call stops between pages; the result is cancelled | The call ends; no resource | `closed` from the tree; the tool ends | Recall; the tree for the read |

## Exceptions with reasons

- The call pins the head it started at: a compaction during the call must not move the page's boundary, so the first `read`'s cutoff is reused for every later page.
- A body that is not needed for the summary is not loaded: the tool counts and titles, not the text, so the index page stays bounded.

## Verification

- A call lists the newest 21 turn positions and pages 20 entries or 8,000 characters; a continuation repeats the original cutoff.
- A text term narrows the origins; a short or non-ASCII query keeps the metadata path; at most eight bodies are loaded per batch.
- A compaction during the call does not move the pinned head; a reload ends the call `closed`.
- The result carries the same text for the same facts regardless of the line's live state.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no fact and folds no state, so it is not a Capability. |
| §2 scopes | Session tool reading a line's facts; the line is an id. |
| §3 orthogonality | No other owner's state is read; the tree answers the facts. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact. |
| §4 rule 6 | The result reaches the model as the tool result. |
| §4 rule 7 | The tool calls the tree's operation; no caller reads the tree's internals. |
| §4 rule 8 | No infrastructure change. |
| §7 values and ids | The line id, the cutoff and the page are data; no object crosses. |
| §7 no proxy | The tree answers the facts; Recall only selects and renders. |
| §7 one interface | No service name; the tool is the surface. |
| §7 cross-language | The tool's execute is a callback; its input and result are data. |
| §7 one change | The implementation task replaces the `TreeCommands`/`Branch` lookup with `timelineTree.read` in one change. |
| ADR 002 D1 | Session scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The tool depends on the tree's read. |
| ADR 002 D4 | No setting; the page budget is the owner's. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

None.

## Binding appendix

No service. The tool registers through the tool contract; its input is `{query?, after?, limit?}` and its result is one `ToolResult` whose content is the rendered page.
