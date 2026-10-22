# ContextCompaction

Status: Implemented on the F1 branch (approved design; implementation notes in §12). Roadmap item F1 ([#72](https://github.com/arcships/dim/issues/72)).
Overview: [Appendix B.1 ContextPreparation row](../overview.md#b1-session-and-branch-capabilities).
Scope: [ADR 002](../../adr/002-plugin-scope.md): Session parts plus supplements to the existing
branch capabilities Loop and ContextPreparation. §12 records where the implementation refines this text.
The trigger, retention and recovery rules follow the
[#128](https://github.com/arcships/dim/issues/128) research
([compaction-trigger](../research/compaction-trigger.md)); its reserve and retention share are
provisional heuristics.

## 1. Summary

**Compaction** replaces the history before a cut with a shorter replacement and keeps the last
turns verbatim. The replaced range is normally everything since the previous compaction point,
together with the previous replacement. From then on, the branch advances on the new result.

Compaction is part of how the Loop controls the context window. It is a Loop step, like a
model call or a tool batch, and it is not tied to a turn. While a compaction step runs, the
Loop does not advance to its next step. Otherwise the context could overflow during
compaction, and new events would be decided on the old context, inconsistent with the
replacement.

Automatic compaction already runs inside a request's preparation (`context/prepare` →
compaction → `model/call`), and the Loop waits for that request's terminal
([context_policy.rs](../../../crates/dim-agent-capabilities/src/context_policy.rs)). It never
runs in production, because the App Server binds `ContextModelHost(None)`
([main.rs](../../../crates/dim-app-server/src/main.rs)).

F1 separates three layers (§2.2) and adds:

1. **Model window facts.** `ContextModel` now carries only window facts: context window, output
   budget, threshold and token count. `ProductContextModel` takes them from the request's adopted
   capability snapshot, with a local token estimate.
2. **A strategy port.** A new `CompactionStrategy` port produces the replacement.
   `ProductSummaryStrategy` is the only strategy F1 implements: it summarizes with the request's
   own model, reaching it through the existing `ProviderConnectionResolver` and `ModelHost`.
3. **A general record.** A completed `context/compaction` carries
   `{retain_from, replacement: Message[], strategy, …}`, where `retain_from` is the id of the first
   Timeline event kept verbatim. There is one replay rule for every strategy: everything before
   `retain_from` is replaced by `replacement`.
4. **One compaction path.** ContextPolicy's compaction step is one function. Automatic
   compaction calls it during preparation, and the Loop's manual step calls it too.
5. **Manual compaction as a Loop step.** `session.context.compact` writes the command
   `context/compact`, and the Loop schedules a compaction step:
   - Inside a turn, the step runs at the next step boundary, before the next request is
     prepared.
   - With no turn running, the step runs at once and writes only its compaction facts.
6. **Compaction state as facts.** Every strategy call is bracketed by two Timeline facts: a
   `context/compaction-start` written just before the call, and exactly one `context/compaction`
   result that refers to it. Clients, history and reload settlement read both (§4.3, §5.7).
7. **Usage query.** `session.context.usage` returns the current budget. It writes nothing.

What does not change: Provider, Aimux, ModelProvider, Timeline, Dispatcher, the event envelope,
rutis and `context/prepare`. F1 adds the event kinds `context/compact` and
`context/compaction-start` and changes `context/compaction` directly; breaking changes are accepted because the software is
unreleased.

### 1.1 Why it is this small

- **Not AuxiliaryModel.** S4's `auxiliaryModel.complete` returns text only. Compaction needs the
  window facts of the model whose context it compacts
  ([auxiliary-model §1.1](https://github.com/arcships/dim/blob/roadmap/s4-provider-purpose-policy-and-auxiliarymod/docs/design/appserver/auxiliary-model.md)).
- **Not inside Provider.** The capability snapshot travels on the `ModelRequest`. Credentials are
  resolved by `ProviderConnectionResolver`, as ModelProvider resolves them for every turn.
  Execution is `ModelHost`.
- **No second state machine.** The Loop sequences every step. ContextPreparation runs the
  compaction function only inside a request preparation the Loop has already handed to it.

## 2. Structure

```
 Process scope (existing, unchanged)
 +-------------------------------------------------------------------+
 |  Aimux: ModelHost        Provider: ProviderConnectionResolver     |
 +----------------------------------^-------------------^------------+
                                    | inject (hard)     |
 Session scope                      |                   |
 +----------------------------------+-------------------+------------+
 |  ContextModelHost(Some(ProductContextModel))   session value (new)|
 |  ProductSummaryStrategy (new part)                                |
 |    provides S:CompactionStrategyHost(Some(summary strategy))      |
 |  ContextMethods (new part)                                        |
 |    provides sessionMethods["session.context"]: compact, usage     |
 |      compact -> TreeCommands.get(b).service::<AgentLoop>().compact()
 |      usage   -> TreeCommands.get(b).service::<ContextPreparation>().usage()
 +---------------------------------^---------------------------------+
            Rpc (C1) dispatches ---'
 Branch scope (one Timeline, one Dispatcher)
 +-------------------------------------------------------------------+
 |  Loop (supplement): sequences steps                               |
 |    model request -> context/prepare (handoff, unchanged)          |
 |    compaction step (manual) -> ContextPolicy::compact_step        |
 |  ContextPreparation (supplement)                                  |
 |    in preparation: ContextPolicy::prepare -> compact_step         |
 |  ContextPolicy::compact_step: retention rule, strategy.compact,   |
 |    validate, append context/compaction, snapshot                  |
 |  Projection (history.rs): replacement + baseline + entries from   |
 |    retain_from                                                    |
 +-------------------------------------------------------------------+
```

`ProductContextModel` needs no service, so it is a plain Session value. ContextMethods is a
separate part from the strategy because branches inject `CompactionStrategyHost`, and its
provider must not depend on `TreeCommands`, which the branch tree provides.

### 2.1 Where the manual step lives

The Loop runs the manual compaction step itself, in a tracked task, by calling
`ContextPolicy::compact_step`. It does not drive ContextPreparation through an event. The reasons:

- **The Loop already holds what the step needs.** Its `Deps` already inject `Timeline`,
  `HistoryRenderer` and `ContextModelHost`
  ([agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs) `LoopPlugin::Deps`);
  F1 adds `CompactionStrategyHost` beside them. The Loop already builds the request from the
  latest `context/configured` in `start_preparation`.
- **Nothing is handed over.** `context/prepare` hands a request to ContextPreparation and then to
  ModelProvider. A manual step makes no model request, so a handoff would only add a second
  command and a second single-flight slot (ContextPreparation's `current`).
- **No race, by construction.** The Loop starts a manual step only when no request of the branch
  is in flight, and it starts no request while a step runs. ContextPreparation calls the step
  only inside the preparation of a request in flight. So there is one sequencer, one function
  and one fact.

### 2.2 Extension points

| Layer | What it fixes | Where | Changes when a new strategy is added |
| --- | --- | --- | --- |
| Mechanism | When compaction runs, and that the Loop holds while it runs: the automatic trigger in preparation, the manual Loop step, the retention rule, validation, commit | Loop, ContextPolicy | No |
| Contract | The record and its replay: `context/compaction {retain_from, replacement, strategy, context_baseline}`, where `retain_from` is a Timeline event id; replay puts `replacement`, then the keyed baseline, then the entries placed by `retain_from` and later events. The record references Timeline events, not rendering details, so another `HistoryRenderer` or strategy still finds the cut. Replay is deterministic, so fork, reload and prompt-cache prefixes see the same messages | `dim-agent-types`, `history.rs` projection | No |
| Strategy | What the replacement is, and the cut within the retention rule | `CompactionStrategyHost` Session value | It is replaced |

**Installing another strategy.** A strategy is installed by replacing the
`CompactionStrategyHost` Session value: the assembly registers another value, or another Session
part that provides it, instead of `ProductSummaryStrategy`.

- ContextPreparation and the Loop depend on the key, not on the strategy's plugin, so no
  optional dependency is added.
- Old records stay replayable because replay never calls the strategy.
- Examples of later strategies: a sliding window with notes, or old tool calls rendered as short
  stubs.

**Not done in F1:** strategies other than the summary strategy, and composing several strategies.

## 3. Data and state

| Item | Owner | Kind |
| --- | --- | --- |
| `context/compact {}` | Loop | New command event (§4.3) |
| `context/compaction-start` | Written by ContextPolicy for its caller | New fact (§4.3) |
| `context/compaction` | Written by ContextPolicy for its caller, or by the settling owner on reload | Existing fact, changed (§4.3) |
| Compaction slot: at most one outstanding command `{sequence, consumed, running: Option<CancellationToken>}` | Loop | Private. Set by `AgentLoop::compact` or by consuming a command; cleared by that command's result. Settled on load (§5.5) |

The Session values hold no state. A resolved connection lives only inside one strategy call.

## 4. Interfaces

### 4.1 Session methods (language-neutral, ADR 002 D5)

`sessionMethods`, Session scope, key `session.context`, provided by ContextMethods
([session-boundary A.1](../contracts/session-boundary.md#a1-method-contributions)). Both
methods are declared with `interruption: report`, `progress: false`, `notifications: []` and
`reply: direct`.

| Method | Mutating | Request | Reply |
| --- | --- | --- | --- |
| `session.context.compact` | yes | `{sessionId, branchId}` | `{status: "completed", sequence}` or `{status: "skipped", reason}` |
| `session.context.usage` | no | `{sessionId, branchId}` | `{through, contextWindow, outputTokens, thresholdTokens, estimatedInputTokens, baselineSource: "provider_baseline" \| "full_estimate"}` |

`session.context.compact`:

- It calls `AgentLoop::compact`, which returns a command sequence `c`: a newly written
  `context/compact`, or the command already outstanding on the branch (§5.4). Concurrent or
  repeated calls therefore join one in-flight compaction.
- It then waits on the branch Timeline for the result that carries its command:
  the first `context/compaction` with `command = c`.
- It maps the outcome: `completed` → `{status: "completed", sequence}`;
  `skipped` → `{status: "skipped", reason}`; `failed` → `compaction_failed`;
  `cancelled` → `compaction_cancelled`.

`session.context.usage` returns the values a preparation at the committed head `through` would
write into `context/budget`, plus `thresholdTokens`.

Cancellation follows session-boundary A.1: an unload interrupts the call, and a caller's
disconnect cancels nothing.

### 4.2 Ports (Rust, runtime-internal)

The existing `ContextModel` port is split in two. Each comes from its own Session value.

| Port | Session value | Methods | F1 implementation |
| --- | --- | --- | --- |
| `ContextModel` (narrowed) | `ContextModelHost(Option<Arc<dyn ResolveContextModel>>)` (existing) | `context_window`, `output_tokens`, `threshold_tokens`, `count_tokens`; `compact` moves out | `ProductContextModel` (§5.1) |
| `CompactionStrategy` (new) | `CompactionStrategyHost(Option<Arc<dyn CompactionStrategy>>)` | `compact(input, cancel) -> Completed {retain_from, replacement, strategy, diagnostics} \| Skipped {reason, diagnostics} \| Err(HostError)` | `ProductSummaryStrategy` (§5.2) |

Both values are bound explicitly in every profile, as `ContextModelHost` is today. The policy
exists when both are `Some`. One `Some` without the other is an assembly defect that
ContextPreparation reports when it loads.

`CompactionInput` is produced by ContextPolicy:

| Field | Meaning |
| --- | --- |
| `history` | The projected entries from the previous `retain_from` on (existing) |
| `checkpoint` | The previous `{retain_from, replacement, context_baseline}` (changed from `cursor` and `summary`) |
| `request`, `model` | The request whose context is compacted, and its `ContextModel` window facts (new) |
| `retain_limit` | `retain_from` of the earliest legal cut whose kept tail fits the retention budget (for a manual step, at least the latest turn's start); the strategy's `retain_from` must be at or before it (new; §5.3) |
| `trigger` | `Threshold`, `ProviderOutputBudgetExhausted` or `Manual` (new) |
| estimates, `provider_budget` | Existing |

### 4.3 Events

| Event | Writer | Consumer | Data |
| --- | --- | --- | --- |
| `context/compact` (new) | Loop, through its public write operation `AgentLoop::compact` | Loop, in sequence order | `{}` |
| `context/compaction-start` (new) | ContextPolicy in `compact_step`, for its caller (Loop step or ContextPreparation) | Loop and ContextPreparation (rebuild), clients | `{turn_id?, request_id?, command?, through, trigger: "manual" \| "threshold" \| "provider_output_budget_exhausted", strategy}` |
| `context/compaction` (changed) | ContextPolicy, for its caller; on reload, the settling owner | Loop (manual step terminal), ContextPolicy (retry reuse), projection, ContextMethods, clients | `{turn_id?, request_id?, command?, start?, through, outcome}` |

`outcome` is one of:

- `completed {retain_from, replacement: Message[], strategy: string, diagnostics, context_baseline}`.
  `retain_from` (a Timeline event id) replaces the renderer-level `cursor`, `replacement`
  replaces `summary: String`, and `strategy` names the producer, for example `"summary"`.
- `skipped {reason, diagnostics}`.
- `failed {error, diagnostics?}`.
- `cancelled {reason}` (new), so every manual step ends with exactly one fact.

| Origin | `turn_id` | `request_id` | `command` |
| --- | --- | --- | --- |
| Automatic (threshold, budget recovery) | the request's turn | the request | absent |
| Manual step inside a turn | the turn | absent | sequence of the `context/compact` served |
| Manual step with no turn | absent | absent | sequence of the `context/compact` served |

**Start and result pairing.**

- `compact_step` writes `context/compaction-start` immediately before it calls
  `strategy.compact`. A compaction that ends before any strategy call writes no start.
- Every start is followed by exactly one `context/compaction` with `start` = the start's
  sequence, and the same `turn_id`, `request_id`, `command` and `through`. Its outcome is
  `completed`, `skipped`, `failed` or `cancelled`.
- A result without `start` exists only for a manual command that ended before any strategy call:
  - `failed` when no request could be built (no `context/configured`, no capability snapshot);
  - `cancelled` when a pending command was dropped by an interrupt or settled on reload.
- Automatic compaction that never reaches the strategy writes neither fact, as today.
- At most one start per branch is unmatched at any time: the Loop never runs a manual step while
  a request is in flight, and ContextPreparation runs one preparation at a time.
- On reload, an unmatched start means a compaction was in flight, and its owner writes the
  `cancelled` result (§5.8). Automatic compaction now also writes a `cancelled` result when it is
  cancelled after its start (interrupt or steer), instead of writing no fact.

**The cut: `retain_from`.** A completed result names the first Timeline event kept verbatim.
Cuts fall only on event boundaries.

- **Replaced range.** The events from the previous completed compaction's `retain_from`
  (inclusive; or from the branch start) up to this `retain_from` (exclusive), together with the
  previous `replacement`. A reader derives it from the two records alone.
- **Validity.** No tool call started before `retain_from` may have its result at or after it.
  The keyed baseline is the authoritative in-effect state just before `retain_from`, as today.
- **Why an event id and not the old cursor.**
  - The old `cursor` was a `HistoryEntry` id such as `<event id>:block:<i>`, which depends on
    the injectable `HistoryRenderer`'s id scheme. A different renderer would break old records.
  - The record is content that users and other state machines read from history, so it must
    reference Timeline events, not rendering details. Event ids are also stable across a fork's
    inherited prefix.
  - With K-turn retention, the cut is naturally the first event of the earliest retained turn,
    which is an event boundary.
- **Mapping at projection time.** ContextPolicy and `history.rs` map `retain_from` to a position
  in the current rendering: the first rendered entry whose source event is at or after
  `retain_from` in sequence order. For this, `HistoryEntry` carries the id of the event that
  placed it (`source`), so the id scheme of entries no longer matters.

**Replay rule (every strategy):**

- The projected messages are `replacement`, then the keyed `context_baseline` blocks, then the
  history entries placed by `retain_from` and later events.
- `replacement` is stored exactly as produced, so replay never calls a strategy and never
  re-renders a wrapper.
- This changes `context_messages` and ContextPolicy's `project` (which today push
  `summary_message(summary)`) and `CompactionCheckpoint` (`replacement` instead of `summary`).
- The history snapshot state stores `retain_from` instead of the entry-id `compacted_through`,
  and the replacement is read from the anchoring fact. The snapshot interpreter version is
  bumped. Older snapshots are bypassed, which is harmless because snapshots are only a cache.
- History-side cut checks (`validate_cut` and the projection's cursor lookup) compare event ids
  instead of entry ids. The snapshot anchor is checked by the snapshot's owner, not by Timeline
  (below).

**Snapshot anchoring without business content (#130).** Today, Timeline core and both stores
read the content of `context/compaction` to anchor projection snapshots:

- `Timeline::context_with_checkpoint`
  ([timeline.rs](../../../crates/dim-agent-runtime/src/core/timeline.rs)) requires
  `EventKind::ContextCompaction`, compares `data.through`, and matches `Completed { cursor }`
  against `checkpoint.state.compacted_through`.
- `MemoryTimelineStore::save_snapshot`
  ([memory_timeline_store.rs](../../../crates/dim-agent-runtime/src/memory_timeline_store.rs))
  rejects an anchor that is not a completed compaction.
- The SQLite store ([storage/timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs))
  does the same on save, and its lookup SQL filters `e.kind='context/compaction'` and
  `status='completed'`.

This contradicts the principle that Timeline checks only the envelope and reserved core domains
([ADR 001](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)), not content, and it
would make every change to the compaction record touch core code. F1 moves the check to its owner:

| Part | Checks after F1 |
| --- | --- |
| Timeline (`context_with_checkpoint`) and both stores | Structure only: the anchor event exists at the snapshot's sequence with the snapshot's event id, the interpreter tag matches, and the sequence is within the cut (plus the existing resume-position checks on the decoded checkpoint). No `EventKind` or outcome matching, and no business kind or status in SQL |
| Snapshot owner (`HistoryRenderer`, whose snapshots ContextPolicy saves) | Anchor validity, through the interpreter callback Timeline already calls. The callback now also receives the anchor event: `decode: fn(&RecoverySnapshot, &AgentEvent) -> Option<ProjectionCheckpoint>`. It accepts only a completed `context/compaction` whose `retain_from` matches its state and whose `through` precedes the snapshot. `None` is a cache miss, as today |
| Saving | The owner saves snapshots only at its own completed compaction, as it does today. The stores no longer enforce the kind |

Behaviour is unchanged for valid snapshots, and an invalid snapshot is a cache miss, which falls
back to full fact replay.

Readers update with the struct:

- ContextPolicy's retry filter compares `turn_id` with `Some(turn)`.
- Native storage indexes completions by `through` only.
- The frozen v1 samples are not changed. They decode only as v1 events; the native record does
  not accept the old `cursor`/`summary` form, and v1 connections no longer receive
  `context/compaction` (§12).

**Dynamic events (decision A).** `context/compact` and `context/compaction-start` are
`EventKind` variants. #2 does not move them: it carries event types the core does not define as
opaque data with the same envelope, so core events keep their variants and wire form. `context/compact`
is owned by the Loop, and `context/compaction-start` by the owner of the compaction facts.

**Diagnostics (breaking).** `CompactionDiagnosticsRecord` becomes strategy-neutral. The
summary-oriented required fields move into a strategy-owned object:

```text
{ reason: "auto" | "manual", trigger,           // common
  retainFrom: string, retainLimit?: string,    // Timeline event ids
  estimatedInputTokens, estimatedOutputTokens,  // before and after, from count_tokens
  entriesReplaced, modelCallCount, durationMs,
  strategy: { kind: string, ... } }             // owned by the strategy
// summary strategy: { kind: "summary", summaryChars, estimatedSummaryTokens,
//                     chunkCount, compactedToolPayloadCount, droppedMessageCount }
```

ContextPolicy fills the common fields; the strategy returns its own object. The old flat record
is removed, not kept beside the new one. The frozen v1 samples are not changed and keep their
own flat record type in the v1 module (§12).

**Client visibility.** SessionBroadcast forwards every branch event unfiltered
([session_broadcast.rs](../../../crates/dim-agent-capabilities/src/session_broadcast.rs)), but
Rpc's wire filter `ProductEvent::new` in
[tree_protocol.rs](../../../crates/dim-app-server/src/tree_protocol.rs) drops
`context/compaction` today. F1 makes `context/compact`, `context/compaction-start` and
`context/compaction` visible to protocol v3 connections through `ProductEvent::new`, so a
client sees every state from live events or from history. `context/budget` and
`context/prepare` stay hidden.

### 4.4 Rust binding

```rust
// dim-agent-types/src/events.rs
EventKind::ContextCompact(ContextCompactData {})                  // "context/compact"
EventKind::ContextCompactionStart(ContextCompactionStartData {     // "context/compaction-start"
    turn_id: Option<String>, request_id: Option<String>, command: Option<u64>,
    through: u64, trigger: CompactionTrigger, strategy: String })
pub struct ContextCompactionData {
    pub turn_id: Option<String>, pub request_id: Option<String>,
    pub command: Option<u64>, pub start: Option<u64>, pub through: u64,
    pub outcome: ContextCompactionResult }
pub enum ContextCompactionResult {
    Completed { retain_from: String, replacement: Vec<Message>, strategy: String,
                diagnostics: Box<CompactionDiagnosticsRecord>,
                context_baseline: Vec<ContextBaselineEntry> },
    Skipped { .. }, Failed { .. }, Cancelled { reason: String } }

// dim-agent-capabilities/src/host.rs
pub trait ContextModel { fn context_window(&self) -> f64; fn output_tokens(&self) -> f64;
                         fn threshold_tokens(&self) -> f64;
                         fn count_tokens(&self, r: &ModelRequest) -> f64; }   // compact removed
pub struct HistoryEntry { /* existing */ pub source: String }   // id of the placing event
pub trait CompactionStrategy: Send + Sync + 'static {
    fn name(&self) -> &str;                            // written into the start, e.g. "summary"
    fn compact(&self, input: CompactionInput, cancel: CancellationToken)
        -> HostFuture<'_, StrategyOutcome>; }          // Completed | Skipped
pub struct CompactionStrategyHost(pub Option<Arc<dyn CompactionStrategy>>);
pub struct CompactionCheckpoint { pub retain_from: String, pub replacement: Arc<[Message]>,
                                  pub context_baseline: Arc<[ContextBaselineEntry]> }

// dim-agent-capabilities/src/context_policy.rs (crate-internal)
impl ContextPolicy { async fn compact_step(&self, step: CompactionStep, cancel: CancellationToken)
                         -> Result<Arc<AgentEvent>, HostError>; }   // committed fact

// dim-agent-capabilities/src/agent_loop.rs
impl AgentLoop { pub fn compact(&self) -> Result<u64>; }  // appends context/compact
impl PublicService for AgentLoop {}
// dim-agent-capabilities/src/context_preparation.rs
impl ContextPreparation { pub async fn usage(&self, cancel: CancellationToken)
                              -> Result<ContextUsage, ContextOpError>; }
impl PublicService for ContextPreparation {}

// dim-agent-capabilities: context_model.rs, summary_strategy.rs, context_methods.rs (new)
pub struct ProductContextModel;                    // ResolveContextModel, no dependencies
pub struct ProductSummaryStrategyPlugin { scope: SessionScope }
//   Deps = (Arc<ModelHost>, Arc<ProviderConnectionResolver>); provides CompactionStrategyHost
pub struct ContextMethodsPlugin { scope: SessionScope }
//   Deps = InSession<TreeCommands>; SessionMethods::provide(ctx, &scope, "session.context", ..)

// dim-app-server/src/main.rs, product profile only (no --models file):
builder.session_value(Arc::new(ContextModelHost(Some(Arc::new(ProductContextModel)))))
       .session_plugin(ProductSummaryStrategyPlugin::new)
       .session_plugin(ContextMethodsPlugin::new)
       .session_methods("session.context");
// --models and mock profiles bind ContextModelHost(None) and CompactionStrategyHost(None)
```

## 5. Flows

### 5.1 ProductContextModel

`resolve(request)`:

1. It requires `request.capability_snapshot`, valid for the request's model. Without it, it
   returns `InvalidRequest` "Model capabilities are not adopted; select the model again", the
   same condition and text as `ProductResolver`.
2. It returns window facts bound to the request:

| Method | Value |
| --- | --- |
| `context_window` | `snapshot.capabilities.contextWindow`, the catalog value with its existing overrides |
| `output_tokens` | `request.config.maxTokens` if set, else `snapshot.capabilities.maxOutputTokens`; never above the latter |
| `threshold_tokens` | `B − reserve`, where `B = context_window − output_tokens` (the input budget) and `reserve = min(max(0.10 × B, 16,000), 0.20 × B)`. The reserve absorbs the estimation error of the next request; it is a provisional heuristic that does not cover every request without a provider baseline ([research §3.3, §6.4](../research/compaction-trigger.md#33-recommendation)) |
| `count_tokens` | UTF-8 bytes of the serialized system prompt, messages and tool declarations divided by 4; each image part counts a fixed 1,600 tokens. Provisional (#128). Used for deltas over the provider's `prompt_tokens`, the first request, `validate` and the reduction check |

### 5.2 ProductSummaryStrategy

`compact(input, cancel)`:

1. **Cut.** `retain_from` is the latest valid event boundary at or before `input.retain_limit`,
   normally `retain_limit` itself. If none exists (a single long turn), it is the latest valid
   boundary that keeps something; under budget recovery (`provider_budget`) it may also be the
   boundary that replaces everything (#169). If there is still none, or nothing lies before it: `Skipped {reason:
   "nothing_to_compact"}`, with no model call.
2. **Summary.** It summarizes the previous replacement (from `input.checkpoint`) plus the entries
   placed by events before `retain_from`.
3. **Result.** It returns `Completed {retain_from, replacement: [summary_message(summary)],
   strategy: "summary", diagnostics}`. The existing `summary_message` wrapper, which marks the
   text as data and escapes it, is applied once here and stored in the record.

How the summary is produced:

- **Request.** One summarizer request per chunk. It uses `input.request`'s provider, model,
  snapshot and config, with `maxTokens = model.output_tokens()`, a fixed summarizer system
  prompt owned by this module, one user message with the transcript, and no tools.
- **Credentials and failures.** The connection comes from `ProviderConnectionResolver` and is
  used only inside this call. The call runs through `invoke_with_recovery`, ModelProvider's
  failure and credential policy. S4 extracts it; if F1 lands first, F1 does the same extraction.
- **Output.** Thinking output is dropped. A tool stop or an empty text is a failure.
- **Transcript.** Role-labelled text: tool calls as name and input, tool results as text, images
  as `[image]`.
- **Chunking.** One call's input budget is `context_window − output_tokens`, checked with
  `count_tokens` on the whole summarizer request (system prompt, carried summary, envelope and
  chunk), the same accounting as every other request; there is no separate character rule. A
  longer transcript is folded in chunks: each call gets the running summary and as many entries
  as fit. An entry that does not fit alone is truncated in the middle by bytes on UTF-8 character
  boundaries, with the marker inside the budget, and counted in `compactedToolPayloadCount`.
- **Cut-off replies.** A reply that stops at `max_tokens` (thinking can use most of the
  allowance) is not a summary. Its chunk is halved at the entry separator nearest the middle (or
  at a character boundary) and folded half by half; a chunk still cut off below 2 KiB fails the
  compaction, so no record keeps a cut-off text (#169). Each half carries the summary of the one
  before it, so its whole request is checked against the budget again before the call; a half
  that no longer fits is halved too, keeping all of its text, and when the carried summary and
  envelope do not fit without any transcript the compaction fails without sending the request.
- **Diagnostics.** The summary object (`kind: "summary"`, `summaryChars`,
  `estimatedSummaryTokens`, `chunkCount`, `compactedToolPayloadCount`, `droppedMessageCount`);
  ContextPolicy fills the common part.

### 5.3 One compaction path (ContextPolicy)

`compact_step` is the existing compaction section of `ContextPolicy::prepare`, extracted:

1. **Retention limit.** The retention budget is `0.25 × B` (provisional, #128). `retain_limit` is
   the `retain_from` of the earliest legal cut that keeps something and whose kept tail fits that
   budget, counted with `count_tokens` on the keyed baseline at the cut plus the entries from the
   cut on. The baseline counts because it is replayed after every cut. This keeps whole recent
   turns when they fit and cuts inside the running turn when it is too large. A manual step cuts
   at the start of the latest turn, or at the token-rule cut when that keeps less. When no cut
   qualifies (one entry or the baseline alone exceeds the budget), there is no limit, and the
   fallback cut of `CompactionInput::cut` applies.
2. **Start.** It appends `context/compaction-start` with the caller's ids, `command`,
   `through`, `trigger` and `strategy.name()`.
3. **Strategy.** It calls `strategy.compact` with `retain_limit`, the legal `cuts` and
   `trigger`. If it is cancelled before the strategy returns, it appends the `cancelled`
   result for the start and returns `Cancelled`.
4. **Validation.** For any strategy, it checks that:
   - `retain_from` is an event of the fixed context and passes the event-based validity rule
     (§4.3);
   - `retain_from` is at or before `retain_limit`; or, when no legal cut is, it keeps
     something, or replaces everything only under budget recovery;
   - the projected request is smaller than before and below the window.

   A failed check turns the outcome into `failed`.
5. **Result.** It appends `context/compaction` with the caller's ids, `command` and `start`, then
   saves the optional snapshot.

Callers:

- **ContextPreparation, inside `prepare`.** At the threshold (once per boundary, as today) or
  under `recovery`. The rest of `prepare` is unchanged.
- **The Loop's manual step.** Trigger `Manual`. The request is built as `start_preparation`
  builds one: the latest `context/configured` settings and system with the projected messages,
  without binding tools.

**Budget recovery.** `validate`, called by ModelProvider before every model call, uses the
trigger's estimate of the request (the provider baseline plus the delta when one exists, else the
full local count) and fails with budget facts when it exceeds `B`; the Loop then retries the request
once with `recovery`. Recovery thus means "the request does not fit the declared budget",
consistent with the trigger. Without a provider baseline the full local estimate and its error
remain (#128).

### 5.4 Manual compaction

**Inside a turn:**

```
      step/model-start {T, R1} ... step/model-output-end {T, R1, completed}
 k    context/compact {}                          writer: Loop (AgentLoop::compact)
      -- Loop consumes k: a request is in flight -> pending
      step/tool-call {T, R1} ... step/tool-result {T, R1}
      -- Loop consumes the tool result (boundary b): pending -> compaction step
 s    context/compaction-start {turn_id: T,       writer: ContextPolicy (Loop's step)
        command: k, through: b, trigger: manual, strategy: "summary"}
 m    context/compaction {turn_id: T, command: k,  writer: ContextPolicy (Loop's step)
                          start: s, through: b, outcome}
      -- Loop consumes m: the step is over -> next_model as usual
      input/consumed* ; step/model-start {T, R2}   Loop (unchanged)
      context/prepare {T, R2, ...} -> model/call   on the compacted context
```

**With no turn running:**

```
 k    context/compact {}                          writer: Loop (AgentLoop::compact)
      -- Loop consumes k: no request in flight -> compaction step at once
 s    context/compaction-start {command: k, through: k,      writer: ContextPolicy
                                trigger: manual, strategy: "summary"}
 m    context/compaction {command: k, start: s, through: k, outcome}   writer: ContextPolicy
      -- Loop consumes m: the step is over -> try_start
      (no turn/start, no turn/end; a driving input applied during the step starts
       a turn now, prepared from the compacted context)
```

**Automatic, for comparison (inside a request's preparation):**

```
      step/model-start {T, R} ; context/prepare {T, R, through: p}       Loop
      context/budget {T, R}                                              ContextPreparation
 s    context/compaction-start {turn_id: T, request_id: R, through: p,   ContextPolicy
        trigger: threshold | recovery, strategy: "summary"}
 m    context/compaction {turn_id: T, request_id: R, start: s, through: p, outcome}
      model/call {T, R}                                                  ContextPreparation
```

**Scheduling rules:**

- **Boundary.** A step boundary is where the Loop would prepare the next request (`next_model`
  without a budget-retry source) or where the turn ends. A pending step starts at the first
  boundary.
  - If the turn ends instead, the step runs right after `turn/end`, with no turn.
  - A budget retry belongs to its request and is not a boundary.
- **Through.** `through` is the sequence of the event whose consumption started the step.
- **One outstanding command.** By the maintainer's decision, repeated requests are an interaction
  concern: deduplication belongs to the interaction boundary, not to the Loop's state machine or
  the Timeline.
  - `AgentLoop::compact` is the only writer of `context/compact`. Under the Loop's lock it checks
    whether a command is outstanding on the branch (written and without a result). If one is, it
    writes nothing and returns that command's sequence, and the caller waits for that command's
    result. Otherwise it writes the command.
  - The Loop's slot holds at most one outstanding command. Its result clears the slot.
  - **Defensive rule.** A `context/compact` the Loop consumes while another command is outstanding
    (one not written through `AgentLoop::compact`, or old data) is settled at once with
    `context/compaction {command: <its sequence>, outcome: skipped {reason: "a compaction is
    already in progress"}}`, without a start. It is not queued.
- **No advance.** While a step runs, `next_model` and `try_start` wait for its fact. Inputs queue
  as usual and are not consumed by the step.

**Steer.** A driving input applied during a step does not cancel it. It joins the next request,
or starts a turn after the step. Interrupt, failure, reload and restart are in §5.8.

### 5.5 What the Loop gains

All in `crates/dim-agent-capabilities/src/agent_loop.rs`:

1. **Subscription.** `context/compact`, `context/compaction-start` and `context/compaction` are
   added. `CompactionStrategyHost`
   is added to `Deps`.
2. **Public write.** `AgentLoop::compact()` returns the outstanding command's sequence, or appends
   `context/compact` and returns its sequence (§5.4). `AgentLoop` gets the `PublicService` marker.
3. **Compaction slot**, at most one outstanding command. On consuming that command, the Loop marks
   the step waiting, and starts it at once if no request is in flight. Another command consumed
   while one is outstanding is skipped (§5.4).
4. **Boundaries.**
   - `next_model` (except with a budget-retry source) and `turn/end` start a pending step instead
     of advancing.
   - `next_model` and `try_start` do nothing while a step runs.
5. **The step task.** It is tracked like the preparation task, builds the request as
   `start_preparation` does, and calls `ContextPolicy::compact_step`.
6. **Step terminal.** On consuming the result of the outstanding command, the Loop clears the
   slot. If its step ran, the Loop resumes: `next_model` inside a turn, `try_start` otherwise.
   Results of other commands (skips, settlements) change nothing.
7. **Interrupt.** A `stop/interrupt` cancels the running step, whose result is `cancelled` with
   its `start`, or writes `cancelled` without `start` for a waiting command. The command is
   settled once.
8. **Rebuild.** The slot is folded from command, start and result pairs:
   - A manual start without a result: the step was running. The Loop writes the `cancelled`
     result with that `start` and `command`.
   - A command with neither a start nor a result: it was waiting. The Loop writes a `cancelled`
     result with that `command` and no `start`. Each command is settled once. An unloading Loop
     starts no further step.

Unchanged: turn and request lifecycles, input consumption, tool batches, steer of model
requests, budget recovery, `LoopSnapshot` and `context/prepare`.

### 5.6 What ContextPreparation gains

- `ContextPolicy::prepare` calls `compact_step`. ContextPreparation injects
  `CompactionStrategyHost`, and its policy exists when both hosts are `Some`.
- `rebuild`: an automatic start (no `command`) without a result is settled first with its
  `cancelled` result, and then the open preparation with the existing reloading terminal.
- `usage(cancel)` reads the committed head and runs the existing estimate. It writes nothing.

### 5.7 Compaction state for clients

Every state is a Timeline fact, so a client sees it from live events or from history at any
time:

| State | Facts | Applies to |
| --- | --- | --- |
| Pending | A `context/compact` with no start or result referring to it | Manual |
| Running | A `context/compaction-start` without its result | Manual and automatic |
| Done | `context/compaction` (`completed`, `skipped`, `failed`, `cancelled`) | Manual and automatic |

- A client that connects mid-compaction reads the branch history (`session.history` or the
  attach replay) and sees an unmatched start, so it shows "running" without any extra channel.
- The start carries `trigger` (manual, threshold or recovery) and `strategy`, and the result
  carries the outcome. The client needs no other state.
- The wire filter change is in §4.3.

### 5.8 Failure, interrupt, reload and restart

| Case | Timeline facts | What the Loop does next | What the client sees |
| --- | --- | --- | --- |
| Strategy error or validation failure, manual inside a turn | start, then `context/compaction {start, failed}` | Continues the turn uncompacted (`next_model`). If the next request exceeds the input budget, the existing budget recovery forces a compaction (fallback cut); if that also fails, the request and the turn fail | Running, then `failed`; reply `compaction_failed` |
| Same, manual outside a turn | start, then `failed` | Nothing more (`try_start` if an input is waiting) | Running, then `failed`; reply `compaction_failed` |
| Same, automatic | start, then `failed` | Unchanged: the request continues uncompacted; under recovery the request fails | Running, then `failed` |
| Manual, no request can be built (no configuration or snapshot) | `failed` without `start` | As above | Pending, then `failed` |
| `stop/interrupt` during a running step (manual or automatic) | `cancelled {reason}` for the start, or `completed` if the strategy had already returned | Manual inside a turn: the turn ends `Cancelled` through `next_model`. Automatic: the request ends `Cancelled` as today | Running, then `cancelled` or `completed`; reply to match |
| `stop/interrupt` while a manual command is pending | `cancelled` without `start` | Turn behaviour unchanged | Pending, then `cancelled` |
| Plugin reload | For each unmatched start, the owner writes `cancelled: "capability reloading"` with that `start`: the Loop for a manual start, ContextPreparation for an automatic one (before its existing reloading terminal). For a pending command, the Loop writes `cancelled` without `start` | No replay. A rebuilt turn ends `Failed` as today | Running or pending, then `cancelled`; a waiting reply is `operation_interrupted` or `compaction_cancelled` |
| Process crash or restart mid-compaction | On the next load, the same settlement as a reload, from the same unmatched starts and commands | Same as reload | Running (from history) until the load, then `cancelled` |

**About crashes and restarts:**

- **Atomic facts.** Timeline batches commit atomically, so a start or a result is either fully
  present or absent, never half written. A crash between them leaves an unmatched start, which
  the next load settles.
- **Snapshots are only a cache.** A crash between the fact and the projection snapshot is
  harmless: projection replays from the fact (principles §4 rule 9).
- **Model work is lost.** The strategy's model work in flight is lost. The next automatic trigger,
  or a new `/compact`, compacts again.
- **No resume of a half-finished compaction.** Model calls are not idempotent and are not replayed
  after a reload anywhere in the runtime. The user's intent may also be stale: the history may
  have grown, or the user may no longer want it.

## 6. Errors

`MethodError` codes (`code -32000` unless noted):

| Code | Method | When | Retry |
| --- | --- | --- | --- |
| `-32602` | both | `branchId` missing or not a string | No |
| `session_invalid_input` | both | Unknown branch | No |
| `session_closed` | both | Session or branch closing | No |
| `context_not_configured` | usage | No `context/configured` in the branch's history | After a model is adopted |
| `context_model_failed` | usage | `resolve` failed (no capability snapshot) | After selecting the model again |
| `compaction_failed` | compact | Fact `failed`: strategy error after the shared failure policy, empty summary, a `retain_from` that fails validation, no reduction, no `context/configured`, or no snapshot | User's choice |
| `compaction_cancelled` | compact | Fact `cancelled`: `stop/interrupt` or reload | User's choice |
| `operation_interrupted` | compact | ContextMethods unloads while waiting; the command is still served or settled | User's choice |

Automatic compaction errors are unchanged: a strategy error becomes a `failed` fact and the
request continues uncompacted; under recovery it fails the request.

## 7. Security

- **Credentials stay request-local.** The connection comes from `ProviderConnectionResolver`
  inside one strategy call and goes to `ModelHost`. It is never stored, returned, serialized or
  logged. Provider is unchanged.
- **No new data egress.** The summarizer sends the Session's own history to the provider and model
  that receive it on every turn.
- **The replacement is untrusted model output.** The summary strategy stores it inside the
  existing `summary_message` wrapper, which marks it as data.
- **No secrets in results.** Facts, replies and diagnostics carry counts, ids, replacement
  messages and `HostError` text, which contains no credential values. No request or summary
  content goes to tracing.
- **Authorization.** Rpc authorizes `sessionId` for Session methods (C1).

## 8. Relationship to existing parts

| Part | Relationship |
| --- | --- |
| Loop | Supplement (§5.5). Budget recovery becomes active in the product profile |
| ContextPreparation, ContextPolicy | Supplement (§5.3, §5.6): `compact_step`, retention rule, validation of any strategy, `usage` |
| Projection (`history.rs`), snapshots | One replay rule using `replacement` and `retain_from`; `HistoryEntry.source`; cut validation and the snapshot state become event-based (interpreter version bump); the decode callback checks the anchor event (#130) |
| Timeline core, memory and SQLite stores | Snapshot anchoring becomes structural only (#130, §4.3) |
| `HistoryRenderer` | Render-time projections that run on every request without a record are not compaction. An example is truncating old tool output by a fixed rule. They belong to the existing injectable `HistoryRenderer`, not to a strategy |
| ModelProvider | Unchanged code. `validate` now enforces the window estimate in the product profile |
| Provider, ProductResolver, Aimux | Unchanged; the summary strategy uses `ProviderConnectionResolver` and `ModelHost` |
| AuxiliaryModel (S4) | Not used. Shares `invoke_with_recovery`; whichever lands first extracts it |
| `sessionMethods` (C1) | Required for the two methods. The host values and the strategy do not need C1 |
| Rpc `ProductEvent` (`tree_protocol.rs`) | Makes `context/compact`, `context/compaction-start` and `context/compaction` visible to v3 connections |
| Turn consumers (clients, future Goal, Hooks) | Unaffected. A manual compaction outside a turn writes no `turn/*` |
| #2 dynamic events | No change: both events stay `EventKind` variants with the same wire form |
| `--models` profile, mock profile | Both hosts `None`: no compaction, and `session.context` is not recorded |
| ManagedProviders plan usage, CLI `/compact` and `/usage` | Not part of F1's acceptance |

## 9. Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (Capability, App Server boundary) | New parts and supplements live in `dim-agent-capabilities`; the App Server only installs them in its product profile. |
| §1 one event bus | The command and the fact are on the branch Timeline and consumed through its Dispatcher. The method's wait reads that Timeline. Clients see the compaction state from the same facts, so there is no side channel. |
| §3 dependency direction | New parts depend on core services. ContextPreparation and the Loop depend on the `ContextModelHost` and `CompactionStrategyHost` keys, which every profile binds explicitly. No optional dependency. |
| §3 changing another module (Loop) | Approved change. The need is the Loop's own control of the context window: compaction is a Loop step like a model call or a tool batch, and the Loop must not advance on a context the replacement supersedes. It closes within the Loop: automatic compaction already runs in its request preparation, and the manual step uses the same function under the same sequencer. |
| §3 extension without change (three layers) | The mechanism (Loop step and trigger) and the contract (record and replay) are fixed. The strategy is a replaceable Session value. A new strategy changes neither the Loop, nor the record, nor replay, and replay of old records never calls a strategy (§2.2). |
| §4 rule 1 (command events) | The client asks the owner of progression with `context/compact`. The Loop decides when to serve it, and the result is one `context/compaction` that references the command. |
| §4 rule 2 (handoff) | Model requests are handed over with `context/prepare` as today. A manual step hands nothing over. |
| §4 rule 3 (before callback; handlers) | No before callback. Loop handlers register the step task and return. |
| §4 rule 4 (no business meaning in core) | Timeline, Dispatcher and facades unchanged. |
| §4 rule 5 (atomic batch) | Each new write is one event. |
| §4 rule 6 (context only from handed-off events) | The replacement reaches the model only through the projection of a completed fact. Pending inputs are not consumed by a step (AGENTS rule 10). |
| §4 rule 7 (public events and services) | `TreeCommands`, `Branch::service` on `PublicService`s, `ProviderConnectionResolver`, `ModelHost`, `sessionMethods`. |
| §4 rule 8 (infrastructure unchanged) | Dispatcher, envelope, rutis, loader, Agent assembly and facades unchanged. There is one infrastructure change (#130): snapshot anchoring in Timeline and both stores drops its knowledge of `context/compaction` and keeps only structural checks, and the owner's decode callback receives the anchor event. The requirement comes from Timeline itself: it must not know event content (principles §4 rule 4; Timeline checks only the envelope and reserved core domains, ADR 001). It closes within Timeline's own contract: a snapshot is a cache with a structural anchor. Valid snapshots behave the same, and an invalid one is a cache miss. |
| §4 rule 9 (only needed facts) | `context/compact` is read by the Loop's later decisions and on reload. `context/compaction` is used by projection. `context/compaction-start` and the visible result are facts by the maintainer's decision, for two reasons. First, they do not only drive a state machine: they are content that users and other state machines need in order to read the history (a UI connecting at any time must see that a compaction is running or how it ended). Second, reload settlement uses them: an unmatched start means a compaction was in flight. The Loop and ContextPreparation rebuild from start/result pairs, which is more precise than commands alone because it tells a running compaction from a pending one and covers automatic compaction. Usage writes nothing. |
| ADR 002 D1 (scope) | Window facts, strategy and methods: Session. Command, step and slot: branch, in the Loop. |
| ADR 002 D2 (no wiring modules) | ContextMethods exposes owners' operations through `sessionMethods`. |
| ADR 002 D3 (dependencies) | Hard dependencies on core only. A Provider or Aimux reload propagates to the strategy part (accepted cost). |
| ADR 002 D4 (configuration) | No configuration; provisional constants are code (#128). |
| ADR 002 D5 (language-neutral contracts) | §4.1 and §4.3 are the contracts; §4.4 is one binding. |

**Impact (public content):**

- The `session.context` namespace.
- The command `context/compact`.
- The changed `context/compaction` (optional ids, `command`, `start`, `cancelled`,
  `retain_from` instead of `cursor`, `replacement` and `strategy` instead of `summary`,
  strategy-neutral diagnostics).
- The fact `context/compaction-start` and the `start` reference on results.
- `context/compact`, `context/compaction-start` and `context/compaction` become visible to v3
  clients.

Internal Rust changes:

- The split ports and `CompactionStrategyHost`.
- `CompactionInput` fields.
- `CompactionCheckpoint {retain_from, replacement}` and `HistoryEntry.source`.
- `history.rs` cut validation and the cursor lookup become event-based. The snapshot state stores
  `retain_from` (interpreter version bump).
- Snapshot anchoring (#130): `Timeline::context_with_checkpoint` keeps only structural checks,
  and its `decode` callback becomes `fn(&RecoverySnapshot, &AgentEvent)`.
  `MemoryTimelineStore::save_snapshot` and the SQLite store's save and lookup drop the
  compaction kind and status checks.
- `PublicService` on `AgentLoop` and `ContextPreparation`.

**Requirements raised to mechanism owners:** none.

## 10. Decisions and open questions

| Item | Decision |
| --- | --- |
| Which model compacts | The request's own model, in the summary strategy; a separate model is deferred |
| Trigger and retention | #128: trigger at `B − min(max(0.10 × B, 16,000), 0.20 × B)`; keep the longest tail, keyed baseline included, that fits `0.25 × B`; manual steps keep at most the latest turn; `validate` compares the trigger's estimate with `B`; `bytes / 4` and 1,600 tokens per image unchanged. The reserve and share are provisional heuristics |
| Manual compaction | A Loop step scheduled by `context/compact`, inside or outside a turn, never creating a turn |
| Where the step runs | In the Loop, calling the one ContextPolicy compaction function (§2.1) |
| Record | `retain_from` (Timeline event id), `replacement` and `strategy` with one replay rule; optional ids, `command`, `cancelled`; changed directly (unreleased) |
| Ports | `ContextModel` (window facts) and `CompactionStrategy` (replacement), each from its own Session value |
| Other strategies, composition | Not done in F1 |
| New event kinds | `context/compact` and `context/compaction-start` as `EventKind` variants |
| `--models` profile | No capability snapshot, no compaction: accepted |
| Provider-reported overflow | Separate Aimux issue, outside F1 |
| Diagnostics | Strategy-neutral now: common counts plus a strategy-owned object (breaking) |
| Compaction state | Facts: pending = command without start, running = start without result, done = result (§5.7) |
| Half-finished compaction | Never resumed; settled as `cancelled` (§5.8) |

Open: the #128 follow-ups outside F1 (a summarizer request that reuses the conversation prefix,
Anthropic cache breakpoints, provider-reported overflow mapped to recovery) and the summary
evaluation of #170.

## 11. Acceptance (flow tests)

The tests run a product-profile App Server against a mock HTTP provider with a small context
window (`crates/dim-app-server/tests`), plus Loop and policy flow tests in
`crates/dim-agent-capabilities/tests`.

1. **Automatic.** Past the threshold, the next preparation writes `context/budget`, then
   `context/compaction-start {trigger: threshold}`, and sends one summarizer request. It commits
   `context/compaction` `completed` with the request's ids, `start`, no `command`, `strategy: "summary"`, a one-message `replacement`, and `retain_from` = the
   earliest legal cut whose kept tail fits `0.25 × B`. The model request starts with that message
   and keeps that tail verbatim. The product window facts give the reserve's cap, floor and ratio
   (32k/8k → 19,200; 128k/8k → 104,000; 1M/128k → 784,800). Many small turns after a large one
   are kept beyond two turns, and a keyed block replayed after the cut keeps fewer of them than
   the same block unkeyed.
2. **Budget recovery.** The retry compacts with `trigger: provider_output_budget_exhausted`.
   With a provider baseline, a request whose local count fits the window but whose baseline
   estimate exceeds `B` recovers; one whose baseline estimate fits does not.
3. **Manual, no turn.** `session.context.compact` writes exactly `context/compact`,
   `context/compaction-start {command}` and `context/compaction {command, start}`, with no
   `turn/*`, `step/*` or `model/call`. The reply is
   `completed`. On a two-turn session the cut is the latest turn's start, or a cut inside that
   turn when its input alone exceeds the retention budget.
4. **Manual, inside a turn.** `step/tool-result` → `context/compaction-start {turn_id, command}`
   → `context/compaction {turn_id, command, start}` →
   the next request's `step/model-start`, prepared from the compacted context.
5. **Replay and strategy replacement.**
   - A Session reloaded after compaction, and a branch forked after it, project the same messages
     as before.
   - A test strategy bound in place of the summary strategy, returning a two-message
     `replacement`, is committed and replayed by the same rule, with no Loop or policy change.
   - The policy rejects a test strategy whose `retain_from` lies inside the retained tail, or
     splits a tool call from its result.
   - A Session compacted with the default `HistoryRenderer` and replayed with a different
     renderer (another entry id scheme) still finds the cut from `retain_from` and projects the
     same retained events.
   - Snapshot anchoring (#130): both stores save and return a snapshot anchored at any existing
     event with the matching id and sequence. The owner's decode callback ignores a snapshot whose
     anchor is not a completed compaction, or whose `retain_from` does not match its state, and
     the context falls back to fact replay with the same result.
6. **Interrupt, reload and restart.**
   - `session.stop` during a step writes `cancelled` (or `completed` if the strategy had
     returned).
   - A plugin reload during a step writes `cancelled: capability reloading` without a second
     strategy call.
   - Killing the App Server process while the summarizer request is held open, then starting it
     again on the same home, leaves an unmatched start and no partial result. The next load
     writes `cancelled: capability reloading` with that `start`. The same holds for an automatic
     compaction. The Session projects the history as
     before the compaction, and a new `/compact` compacts normally.
   - A crash after the fact but before the snapshot save projects the compacted history from the
     fact.
   - A manual compaction failing inside a turn lets the turn continue. When the next request
     exceeds the input budget, budget recovery compacts.
7. **One outstanding command.**
   - Repeated or concurrent `session.context.compact` calls while a command is outstanding: the
     Timeline holds exactly one `context/compact`, one start and one result, and every caller
     receives the same reply.
   - A `context/compact` consumed while another is outstanding (written without
     `AgentLoop::compact`) gets `skipped: "a compaction is already in progress"` at once, with no
     start, and the outstanding compaction completes normally.
   - `stop/interrupt` or a plugin reload with one outstanding command settles it `cancelled` once.
8. **Compaction state for clients.**
   - A v3 client attached to the Session receives `context/compact`, `context/compaction-start`
     (`trigger: manual`) and `context/compaction` live.
   - A client that connects while the summarizer request is held open sees "running" from the
     branch history (an unmatched start).
   - An automatic compaction shows `trigger: threshold` with the request ids.
   - Across every test above (success, skip, failure, interrupt, reload, crash), each
     `context/compaction-start` has exactly one `context/compaction` with its `start`, and no
     result carries a `start` that does not exist.

## 12. Implementation notes

The implementation follows this design. Where the code needed a detail the text did not
fix, the smallest choice was made and is recorded here.

| Item | Implementation |
| --- | --- |
| Start trigger values | `trigger` uses the existing `CompactionTrigger`: `manual`, `threshold`, `provider_output_budget_exhausted` (§4.3 said `recovery`; §4.2 and acceptance 2 already used the latter). |
| Legal cuts | `CompactionInput` also carries `cuts`: every legal cut in order (`retain_from`, the first kept entry, and whether it is within `retain_limit`), computed by ContextPolicy from the event-based validity rule. Candidates are turn starts, events that place entries, and the boundary's last event (which replaces everything). `CompactionInput::cut()` returns the latest cut within the limit or, when none is, the latest cut that keeps something (any cut under budget recovery). A strategy may return any `retain_from`; the policy still validates it. |
| Adoption | `compact_step` validates the strategy's result before it appends the fact, then adopts the committed fact as Timeline middleware left it: the projection, the reduction check and the snapshot use the committed `retain_from`, `replacement` and baseline. A committed cut that cannot be projected fails the preparation, as a committed cursor did before. |
| Diagnostics | `retainFrom` is absent when the strategy found no cut (`skipped`), and `estimatedOutputTokens` is absent when nothing was replaced. |
| v1 | There is no v1-to-native conversion in production code: the frozen samples decode as v1 events only, and `v1::EventRef` has no form for `context/compact`, `context/compaction-start` or the changed `context/compaction`, so v1 connections no longer receive compaction facts. The old flat diagnostics type moved into the v1 module. |
| Loop subscription | The Loop subscribes to `context/compact` and `context/compaction`; starts are folded only at rebuild, where they tell a running step from a pending command. |
| One outstanding command | Maintainer's decision during review: repeated requests are deduplicated at the interaction boundary. `AgentLoop::compact` joins an outstanding command instead of writing another, the slot holds at most one command, a command consumed while another is outstanding is skipped, and the waiter matches its own command id (§4.1, §5.4, acceptance 7). This replaces an earlier active/pending split, so commands can no longer interleave with a running step. |
| Snapshot anchor | Timeline keeps the structural checks of §4.3. The old `resume ≤ through` check needed the anchor's content; it is replaced by the structural "resume precedes the anchor on the anchor's branch", and the owner's decode checks `through` before the anchor. |
| Native storage example | The record example in [native-storage-format](../appserver/native-storage-format.md) uses the changed record and the v3 interpreter. |
| Summarizer budget | Review fix: chunk packing and truncation count the whole summarizer request with the window facts' `count_tokens` against `context_window − output_tokens` (§5.2), so CJK-heavy history no longer exceeds the window; `product_summary.rs` checks every summarizer request. |
| Acceptance | Acceptance 1, 3, 6 (crash and restart) and 8 run in the binary flow test `transport_flow/compaction.rs`. Acceptance 2, 4, 5 and the reload, interrupt and failure cases of 6 run in `dim-agent-capabilities/tests/context_flow` (`manual_compaction.rs` and the existing compaction tests). The crash between fact and snapshot is covered by the snapshot-failure test in `compaction_events.rs`. |
| Trigger, retention and recovery (#128) | `ContextPolicy` keeps the estimate of the request its `prepare` returned (a full count after a completed compaction), so `validate` checks the same request without reading the context again; a request it did not prepare reads the context once. Each kept entry is counted once with `count_tokens` and summed, so the per-message separators of a joint count are left out. The trigger values are in `transport_flow/compaction.rs` and `product_summary.rs`; retention and recovery in `context_flow/retention.rs`; the manual cut in `manual_compaction.rs`. |
| Dead zone and cut-off summaries | #169: the retention fallback no longer depends on budget recovery, so a single long turn compacts at its latest cut that keeps something; only recovery may replace everything. `fallback` is removed from `CompactionInput`. A summarizer reply stopped at `max_tokens` is retried in halves and fails at the smallest chunk instead of being kept. |
