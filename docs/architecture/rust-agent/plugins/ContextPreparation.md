# ContextPreparation

Owner: [context_preparation.rs](../../../../crates/dim-agent-capabilities/src/context_preparation.rs)
and [context_policy.rs](../../../../crates/dim-agent-capabilities/src/context_policy.rs).
Prepares a fixed-source model request, enforces context policy and provides the
atomic ConfigurationPublisher used by Prompt.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:HistoryRenderer` | Render supported history for preparation/budget checks. |
| `S:ContextModelHost` | Optional window facts for context policy and recovery. |
| `S:CompactionStrategyHost` | Optional compaction strategy; the policy exists when both hosts are bound. |
| `S:PrepareContextHost` | Host preparation hook. |
| `S:GuideEntries` | Register the digest entry while a strategy is bound. |
| `B:TimelineIdentity` | Identify configuration publication scope; Session and branch ids of the `context.prepare` span. |
| `B:Dispatcher` | Replay and receive preparation/steer/interrupt transitions and report faults. |
| `B:Timeline` | Read fixed context, publish handoff and configuration facts. |
| `B:BranchLifecycle` | Refuse a guide adoption on a closing branch. |

## Events and lifetime

Consumes `context/prepare`, `stop/interrupt`, `step/model-start`, `input/applied`,
compaction start/result and turn start/end. Produces `model/call` on success or
`step/model-output-end` on failure, with `context/compaction-start` and one `context/compaction` around
each strategy call. `usage` reads the head's budget without writing. Its publisher commits prompt changes, adopted inputs and
`context/configured` atomically; it composes the System at every adoption that
publishes a configuration. A change that renders the System composes the new
source text, and a model- or tool-only or guide update re-renders the guide over
the source text the configuration records (`system_source`), so a writer that
loads has its entry in force at that adoption. A guide adoption publishes only
when the composition over the entries in force differs from the adopted System,
with the settings unchanged, and nothing while the branch is closing: the phase
is read before the context and checked again in the commit's register step. A
configuration written before the source was recorded keeps its adopted System
([Prompt §6.3](../../../design/plugins/Prompt.md#63-contextpreparation-the-entries-and-the-published-system)). Preparation is single-flight, tracked,
cancelled by stop/steer and joined during cleanup; a steer during automatic compaction waits
for its result before replacing the request
([ContextCompaction §5.3](../../../design/plugins/ContextCompaction.md#53-one-compaction-path-contextpolicy)).
Cleanup aggregates failures.
Replay settles unmatched handoffs and automatic compaction starts without
rerunning host work. The mock profile
has no context resolver. Completed compaction facts remain authoritative across
checkpoint selection. Recovery snapshots are built from the final adopted cut,
saved separately after fact publication, and selected by interpreter compatibility.
HistoryRenderer owns both snapshot encoding and decoding; the default v2 codec
checks payload and anchor integrity before accepting state and bypasses unprotected
v1 caches. Snapshot construction, encoding or save failure keeps the successful
summary and falls back to fact replay; it does not repeat model work. Trusted
builders must independently preserve full-replay semantics; a checksum is not
proof of semantic correctness or authorization.

The compaction replacement is the policy's, so the part registers the
`dim-digest` entry of the input guide from its own `apply`, scoped to its
branch and only while a strategy is bound: without one the policy compacts
nothing and no digest can be written
([Prompt §4](../../../design/plugins/Prompt.md#4-the-built-in-entries-and-their-writers)).

## Tests

[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs):
`restoring_each_preparation_handoff_settles_without_replaying_work`,
`before_modified_request_is_budget_checked_before_native_model_invocation`;
[checkpoint_projection.rs](../../../../crates/dim-agent-capabilities/tests/context_flow/checkpoint_projection.rs).

<!-- generated:plugin ContextPreparation — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:CompactionStrategyHost`、`S:ContextModelHost`、`S:GuideEntries`、`S:HistoryRenderer`、`S:PrepareContextHost` |
| 提供 | `B:ConfigurationPublisher`、`B:ContextPreparation` |
| 被注入于 | `ModelProvider`、`Prompt` |
<!-- /generated -->
