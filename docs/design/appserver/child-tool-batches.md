# Child tool batches across clients

Status: Draft. Delivery status is tracked in [#1326](https://github.com/arcships/dim/issues/1326), not here.
Scope: App Server protocol and the clients of a Session.
Issue: [#1326](https://github.com/arcships/dim/issues/1326).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

Code Mode records a child tool call as a `step/tool-call` and its result as a `step/tool-result`,
both carrying the outer call id in the optional `AgentEvent.parent_call_id`. The Timeline, storage
and ToolExecutor contracts are already specified in
[CodeMode](../plugins/CodeMode.md) and [ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md).
This document owns what a client is allowed to see and what a v4 client must do with it.

The problem is not whether Code Mode is installed by default. A child event is a new envelope
shape; a client that does not understand `parent_call_id` will render a child as a top-level tool
call and produce an orphan row. Opt-in only hides that failure. The protocol and the client
behavior must change before Code Mode can be on for a client.

## 1. Scope and non-goals

This design owns:

- the protocol boundary for child tool batches;
- which protocol versions receive child events;
- the minimum TUI, CLI and web behavior for those events;
- the npm client declaration;
- the App Server gate that keeps Code Mode away from clients that cannot consume child batches;
- the `AgentLoop::rebuild` and Recall requirements that are part of the same child-event contract.

This design does not own:

- Code Mode execution, child batching, effects, cancellation or reload; those stay in
  [CodeMode](../plugins/CodeMode.md) and [ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md);
- the native store format; v37 carries the envelope property and v38 stores protocol 4;
- rich child-detail UI. A client may hide child details; it may not misclassify them;
- dim-electron's UI implementation; that repository consumes the protocol at its own release
  cadence.

## 2. Own contract

### 2.1 Protocol version 4

`PROTOCOL_VERSION` becomes `4`. Version 4 is the first version that may receive an event with
`parent_call_id`. Versions 1, 2 and 3 keep their existing event allowlists and never receive a
parented event.

The App Server applies one visibility rule at the wire boundary:

```text
ProductEvent::new(event, version):
  if event.parent_call_id is Some and version < 4:
    return None
  ... existing per-kind allowlist ...
```

The rule applies to both live `session.event` notifications and `session.history`. A v3 history
page skips parented events; its `through`/`next_after` cursors still advance over them. A v4 page
includes them. No client needs to know that a skipped event existed.

Version 4 is additive for fields and restrictive for events: `parent_call_id` is optional on the
v4 envelope; an outer event has no field. v1 and v2 encodings remain unchanged; v3 continues to
omit the field and now also omits parented events.

### 2.2 What a v4 client must do

A v4 client receives two kinds of tool facts:

- top-level `step/tool-call` / `step/tool-result` with no `parent_call_id`;
- child `step/tool-call` / `step/tool-result` with `parent_call_id` naming the outer
  `ToolCall.id`.

The minimum behavior is:

- a child event is never rendered as a top-level tool card;
- a child result is attached to the parent card when the parent is in the loaded window;
- if the parent is not in the window, the child is hidden from the visible list; it is not
  promoted to a top-level row;
- child details (arguments, result, timing) are optional; a client may show only a count or a
  summary;
- a pending child `ask_user` or child approval is reachable from the parent card; a client that
  cannot surface it must not offer Code Mode.

A v4 client may choose not to expose child details at all. The only mandatory rendering rule is
no orphan rows.

### 2.3 UI shape and the current renderer

The current CLI draws one `ToolGroup` per model request. Collapsed is one row; `summary` adds one
row per call; `expanded` adds the call rows and their details. The disclosure is `▸` or `▾`; the
outcome marks are `✓`, `✗` and the running mark; sections already use a header at indent 2 and
their rows at indent 3. A child batch currently has no parent in `ViewRow`, so an unchanged client
would open a second top-level group. That is the rendering bug this design removes.

The v4 shape adds child rows to the existing group. It does not show the code body and does not add
a second group or a second disclosure:

```text
✓ Ran code · 4 tool calls ...................................... ▸
```

```text
✓ Ran code · 4 tool calls ...................................... ▾
  ✓ Read CodeMode.md
  ✓ Grep parent_call_id
  · Ran cargo test -p dim-codemode
  · ask_user  Which line?
```

```text
✓ Ran code · 4 tool calls ...................................... ▾
  ✓ Read CodeMode.md
    result: 455 lines
  ✓ Grep parent_call_id
    result: 7 matches
  · Ran cargo test -p dim-codemode
    running 42s
  · ask_user  Which line?
    [1] main  [2] child  [c] custom
```

If the same request also calls an ordinary tool, the header says `Ran code, ran 1 command` and the
ordinary tool keeps its existing section. The code body is not rendered: there is no `Program` or
`Code` block in the transcript and no second `▸`/`▾` target. The outer call is the group header;
expanded shows only the child calls and their details.

This reuses the existing header, summary rows, section indent and outcome marks. It does not add a
new card style or a second transcript. A pending `ask_user` uses the running mark; the question
itself uses the existing question surface. A child approval uses the existing approval surface and
names `child <call> of codemode <parent>`.

Implementation consequence: `ViewRow` carries the parent call id and the projection folds child
rows into the outer request group; changing only the renderer would still leave the child as a
separate group. The `codemode` input is omitted from the drawn group.

### 2.4 TUI and CLI

The CLI projection keeps one tool row per call id. A child event is associated by
`(request_id, parent_call_id)`; the child call is a nested row of the parent `codemode` row.

- A child `step/tool-call` with no known parent is dropped, not inserted as a new top-level row.
- A child `step/tool-result` with no matching child row is dropped.
- A child `ask_user` is shown under the parent; its pending question is still answerable through
  `session.question.respond`.
- A child approval is shown under the parent; its approval id and child call id are preserved.
- The TUI uses protocol version 4 for new Sessions. A v3 TUI is not expected to show Code Mode
  because the App Server does not offer it to a v3 connection.

The detailed row layout remains a TUI concern. The protocol only supplies the parent identity and
the existing child facts.

### 2.5 Web

`web/src/rpc/protocol.ts` uses protocol version 4. `web/src/rpc/read.ts` reads the optional
`parent_call_id` into the event view. For this delivery the chat projection hides a child tool row
instead of promoting it; the parent card remains the outer `codemode` row. Grouping child detail
inside that card can follow without a wire change. A child event whose parent is outside the
current history window is hidden from the card list. A child `ask_user` keeps its pending question
surface.

The web entry manifest and the App Server's exact-version web check move to 4 together; an old web
bundle is refused rather than served an event shape it cannot read.

### 2.6 App Server Code Mode gate

The product profile may install `dim_codemode`, but a Session created by a connection below
protocol 4 must not see the `codemode` tool. This is a tool-exposure rule, not an event filter:

- a v3 or older `session.create` records `codemode` in `SessionConfig.excluded_tools`;
- `ToolRegistry` removes an excluded name even when `SessionConfig.tools` is absent, so the
  default all-tools path cannot bypass the gate;
- an explicit v3 request that names `codemode` is silently excluded, not rejected;
- a v4 connection gets the tool when the plugin is installed and the request asks for it.

A v3 client must not be able to start a Code Mode program whose child `ask_user` it cannot answer
and whose child tool cards it cannot group. Keeping the tool out of v3 Sessions is the smallest
enforcement point.

The exclusion belongs to the Session, not to the connection that opens it: a Session created
below 4 keeps `codemode` in its stored `excluded_tools`, so a v4 connection that opens it still
does not get Code Mode, and a fork of it records the source's version and keeps the exclusion.

That is one case of the rule the lower bound relies on: a connection may write to a Session
recorded at an older version only facts that version's projection can carry, and anything else
it writes must also raise the recorded version. Code Mode satisfies it because the exclusion is
recorded in the Session's configuration, not projected to the client; an event shape the older
version cannot read does not, which is why a new event kind needs a protocol version.

### 2.7 npm client protocol

`npm/client-protocol.d.ts` adds:

```ts
export interface AgentEvent {
  // ...
  parent_call_id?: string;
}
```

The field is optional and only appears in v4 events. A v3 client still compiles and still receives
the v3 projection without it.

### 2.8 AgentLoop rebuild

`AgentLoop::rebuild` is a recovery path, not a client feature, but it consumes the same events. It
must select `parent_call_id` and ignore parented `step/tool-call` / `step/tool-result` events when
it restores the model request. A child call shares the outer `turn_id` and `request_id`; without
the filter a child call overwrites `request.call_event_id` and a child result clears the request,
so a reload between the child result and the outer result settles the turn as failed or attributes
the outer result to the wrong batch.

The live `on_event` path already ignores parented events; rebuild must match it. This is an
internal correctness fix, independent of which clients receive the events.

### 2.9 Recall

Recall groups model tool-use events. Child calls are `step/tool-call` facts, not model tool-use
events. For this delivery, Recall does not claim child grouping; the Code Mode design's Recall
claim is removed until a separate child-call projection exists. A v4 client sees child facts
through `session.history`, not through Recall.

If child Recall is required, it must be a separate design: it needs a parent/child projection and
a decision about whether child arguments are recallable at all.

## 3. Events and state

No new Timeline event kind or state is introduced. `parent_call_id` is already persisted in the
v38 envelope; child events keep `context.render = false` and `context.state = false` as specified
by [CodeMode](../plugins/CodeMode.md). The only state change is the negotiated protocol version of
a connection.

A v3 connection's history and live stream are the same projection as before: child events are
filtered at the wire boundary. A v4 connection sees the stored facts. Storage and recovery remain
version-independent.

## 4. Dependencies

| Dependency | Scope | What this design requires |
| --- | --- | --- |
| App Server transport | process | `PROTOCOL_VERSION = 4`; version-aware `ProductEvent` filtering; web manifest version 4. |
| App Server Session creation | process | omit or reject `codemode` for connections below protocol 4. |
| CLI/TUI projection | client | protocol 4; parent/child grouping; no orphan rows; child question/approval under parent. |
| Web RPC/read | client | protocol 4; `parent_call_id` in the event view; parent grouping; hide orphaned child. |
| npm client protocol | package | optional `parent_call_id` on `AgentEvent`. |
| AgentLoop | Session | filter parented control events in `rebuild`. |
| Recall | Session | no new child claim in v1; separate follow-up if required. |
| dim-electron | external | stays v3 and therefore sees no child events and no Code Mode until it upgrades to v4. |

## 5. Cross-module requirements

- **App Server:** bump `PROTOCOL_VERSION` to 4, apply the parent filter in `ProductEvent::new`,
  update the exact web manifest check, and gate `codemode` tool exposure by connection version.
- **CLI/TUI:** negotiate v4, group child events under `parent_call_id`, and keep pending child
  questions/approvals reachable.
- **Web:** negotiate v4, parse `parent_call_id`, group or hide child events, and update the web
  manifest version.
- **npm:** declare the optional envelope field.
- **agent-capabilities:** select and filter `parent_call_id` in `AgentLoop::rebuild`.
- **docs:** remove the Recall child-grouping claim from CodeMode until Recall implements it.

## 6. Rule check

This is not a plugin part and owns no process, Session or branch state. It adds no Capability and
no Timeline event. It is a protocol and client projection change.

| Rule | Answer |
| --- | --- |
| §1 Capability | Not applicable: no behavior producing or consuming critical events is added; child events already exist. |
| §4.1 scope | App Server owns the wire projection; TUI/CLI/web own rendering; AgentLoop owns recovery. |
| §4.2 facts | Reuses existing child `step` facts; no new fact. |
| §4.3 handlers | Does not add handlers. |
| §4.4 state | No business state; connection version is transport state. |
| §4.5 atomic batches | Does not change batches. |
| §4.6 context | Child events remain non-context; this design only changes client visibility. |
| §4.7 public interaction | Uses existing `session.event`, `session.history` and `session.create` interfaces. |
| ADR 002 D1–D5 | No new part, configuration or language contract. |

## 7. Binding appendix

Exact spellings for the implementation:

```rust
pub const PROTOCOL_VERSION: u32 = 4;

impl ProductEvent {
    pub fn new(event: Arc<AgentEvent>, version: u32) -> Option<Self> {
        if event.parent_call_id.is_some() && version < 4 {
            return None;
        }
        // existing per-kind allowlist
    }
}
```

```ts
export const PROTOCOL_VERSION = 4;
export interface AgentEvent {
  // ...
  parent_call_id?: string;
}
```

The CLI and web projections use the existing event view and add a `parentCallId` field. They do
not add a new event type.

## 8. Compatibility and rollout

1. App Server: v4 protocol, parent filtering, web manifest 4, Code Mode tool gate.
2. CLI/TUI and web: v4 negotiation and parent grouping.
3. npm: optional field.
4. dim-electron: remains v3; it receives no child events and is not offered Code Mode. A later
   dim-electron v4 release may add child rendering.
5. The product install may stay on because v3 connections never see child events or the tool.

A v4 client that has not implemented grouping is a client bug, not a server fallback. It must
follow §2.2: group or hide, never promote.

## 9. Testing

- `ProductEvent::new` returns `None` for a parented event at v3 and `Some` at v4.
- Live `session.event`: a v3 client never receives a child event; a v4 client does.
- `session.history`: v3 pages skip child events and still advance cursors; v4 pages include them.
- CLI projection: a child call/result does not create a top-level row; a child result attaches to
  its parent; a child with no parent is dropped.
- Web read: `parent_call_id` is parsed; a child card is grouped under the parent; an out-of-window
  child is hidden.
- Session creation: a v3 connection cannot obtain `codemode`; a v4 connection can.
- `AgentLoop::rebuild`: a reload with a child result before the outer result restores the outer
  request and settles the turn correctly.
- npm TypeScript: the optional field compiles against a v4 event and a v3 event.
- dim-electron v3 compatibility is verified against the filtered stream.

## 10. Ablation review

The version bump and one filter in `ProductEvent::new` replace a per-client feature negotiation
and a second event type. The child facts, `parent_call_id`, v37 storage and context flags already
exist; this design adds no persistence. The only new work is the version boundary, the client
grouping and the tool gate. Rich child UI is intentionally optional.

If a future client can consume child events without a protocol bump, it can advertise a capability
instead; that is not needed for the first v4 rollout.

## 11. Decisions

- **D1:** Use protocol version 4, not a per-client feature flag. Older clients connect and receive
  the old projection; v4 clients receive child events.
- **D2:** v3 clients do not receive child events and are not offered `codemode`. Hiding only the
  cards but allowing the tool would leave child questions and approvals unreachable.
- **D3:** Child rendering is a client decision. Grouping or hiding is mandatory; detail is not.
- **D4:** The product install may remain on because the gate is at Session tool exposure and wire
  visibility.
- **D5:** Recall child grouping is out of scope for v1; remove the claim from CodeMode until a
  separate projection exists.
