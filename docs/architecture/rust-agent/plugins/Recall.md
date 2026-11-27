# Recall

The Recall plugin registers Recall in every Session: the `recall` tool tells
the model the history of its own branch. Design:
[Recall](../../../design/plugins/Recall.md).

## Recall

Owner: [recall.rs](../../../../crates/dim-agent-capabilities/src/recall.rs).
Registers the Session `recall` tool, which reads its own line through the
narrative projection,
[narrative.rs](../../../../crates/dim-agent-capabilities/src/narrative.rs), in
the second person. Collaboration's `agent read` reads a member's line through the
same projection in the third person
([Recall §3.8](../../../design/plugins/Recall.md#38-person-and-other-readers)).
Each call pins the read branch head and queries through `Branch::query`. The index selects the newest 21 turn positions
and streams their narrow control fields. It reads only opening input titles and
counts tool starts; response bodies are not needed for turn summaries.

Search pages at most 128 origin positions at a time. A supported text term uses
`TextCandidate` to narrow input, reply and tool origins through their identities;
Recall verifies the rendered business text. Short, non-ASCII and fixed-label
searches retain the metadata candidate path. At most eight candidate origins have
their related bodies loaded together. Rejected bodies are released after that
batch; only the 21 result lines needed for pagination remain.

Opening one thing reads its identity and related facts. Opening a turn resolves
its boundaries, then pages origins and loads their bodies in batches of eight.
All paths share the pinned cutoff. No path retries by loading the complete chain
into memory. Storage can still scan metadata when no safe text index applies;
this is not a constant-time search guarantee.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Resolve the calling branch and read its pinned history per call. |
| `S:ToolRegistry` | Register `recall`, until this part unloads. |

### Events and lifetime

Reads history; writes no Timeline event, holds no `StateCell`, subscribes to
nothing and starts no task. Each call folds the pinned events into one origin
event per thing and the later events a join id attaches to it, and tells only
the kinds the design lists; every other kind, core or opaque, is skipped
without decoding its data, so a new kind needs no change here. Counting
follows context assembly: an input counts once it is consumed, a reply counts
when its output completed or as the partial text an interruption left, and a
tool result is told as its `content`.

The pinned head is the whole call: a compaction during the call cannot move
it and the pages all read at that head. A facade read while the Session reloads
returns `HostError::Cancelled("capability reloading")`, any other facade error
is a failed call, and the call's cancellation token is checked between pages and
before rendering, as `wait` does. Pages are 20 entries, or 8,000 characters of
one thing in full; every variable part of a line is cut at 120 characters, so
a line stays at most 600 characters. The tool returns text the model reads and
`structuredContent` for UI and persistence only.

### Tests

[recall_flow.rs](../../../../crates/dim-agent-capabilities/tests/recall_flow.rs)
holds the acceptance flows of the design's §14: the turn index and its pages,
a turn, one thing in full with CJK paging, the allowlist against what context
assembly places, search, compactions, call endings, branch and Session forks,
line bounds, the read-only preset, a long turn, interrupted outputs, the
argument errors, a 120-turn history whose index, search and newest thing
answer from bounded pages, and a child branch paging through its inherited
prefix.
[storage_flow/recall.rs](../../../../crates/dim-app-server/tests/storage_flow/recall.rs)
compares the production SQLite search and index with the complete narrative,
including a search term split across durable output deltas and fallback terms.
[transport_flow/recall.rs](../../../../crates/dim-app-server/tests/transport_flow/recall.rs)
finds `recall` in the tool table of both the product and the `--models`
profile.
