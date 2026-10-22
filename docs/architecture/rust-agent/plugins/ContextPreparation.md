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
| `B:TimelineIdentity` | Identify configuration publication scope; Session and branch ids of the `context.prepare` span. |
| `B:Dispatcher` | Receive preparation/steer/interrupt transitions and report faults. |
| `B:Timeline` | Read fixed context, publish handoff and configuration facts. |

## Events and lifetime

Consumes `context/prepare`, `stop/interrupt`, `step/model-start`, `input/applied`,
turn start/end. Produces `model/call` on success or `step/model-output-end` on
failure, with `context/compaction-start` and one `context/compaction` around
each strategy call. `usage` reads the head's budget without writing. Its publisher commits prompt changes, adopted inputs and
`context/configured` atomically. Preparation is single-flight, tracked,
cancelled by stop/steer and joined during cleanup; cleanup aggregates failures.
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

## Tests

[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs):
`restoring_each_preparation_handoff_settles_without_replaying_work`,
`before_modified_request_is_budget_checked_before_native_model_invocation`;
[checkpoint_projection.rs](../../../../crates/dim-agent-capabilities/tests/context_flow/checkpoint_projection.rs).

<!-- generated:plugin ContextPreparation — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:CompactionStrategyHost`、`S:ContextModelHost`、`S:HistoryRenderer`、`S:PrepareContextHost` |
| 提供 | `B:ConfigurationPublisher`、`B:ContextPreparation` |
| 被注入于 | `ModelProvider`、`Prompt` |
<!-- /generated -->
