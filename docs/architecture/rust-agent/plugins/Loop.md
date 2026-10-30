# Loop

Owner: [agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs).
Coordinates turns, request preparation and model/tool steps from adopted inputs;
exposes loop state without itself executing model or tool effects.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:HistoryRenderer` | Render supported context history for requests. |
| `S:ContextModelHost` | Window facts; with the strategy, context recovery and manual compaction are available. |
| `S:CompactionStrategyHost` | The compaction strategy its manual step calls through ContextPolicy. |
| `S:PrepareModelHost` | Prepare the request before context/model execution. |
| `B:ToolBinding` | Pin and release request tool handlers. |
| `B:Dispatcher` | Replay state, then consume step outcomes, inputs and interrupts. |
| `B:Timeline` | Commit turn/step transitions. |
| `B:TimelineIdentity` | This branch's id selects the guide entries in force. |
| `S:GuideEntries` | Compose the System it places on a host-prepared request. |

## Events and lifetime

Consumes `input/applied`, `input/consumed`, `stop/interrupt`, turn boundaries,
`step/model-start`, tool-use start/use, model-output-end, tool-start/completed/result,
`context/compact` and `context/compaction`.
Produces turn start/end, input consumption, model-start, `context/prepare`,
tool-call and recovery terminal facts. `AgentLoop::compact` writes
`context/compact`; the Loop serves it as a compaction step at its next step
boundary, or at once with no turn, and starts no request or turn while the step
runs (ContextCompaction §5.4). An interrupt cancels a running step and settles a
pending command; reload settles both as `cancelled`. Preparation work is tracked; handlers
schedule it without waiting for host I/O. Cleanup cancels and joins work.
Replay reconstructs pending inputs/turns and coordinates settlement of orphaned
requests rather than repeating external effects. Reload-settled tool results
fail the turn. An idle append alone starts no turn. Domain-specific history must
not become a second input-adoption path.

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs),
[interrupt_flow.rs](../../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs),
[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs)
cover turn progression, interruption, pending-input restore and the manual
compaction step.

<!-- generated:plugin Loop — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding`、`S:CompactionStrategyHost`、`S:ContextModelHost`、`S:GuideEntries`、`S:HistoryRenderer`、`S:PrepareModelHost` |
| 提供 | `B:AgentLoop`、`B:StateCell<LoopSnapshot>`、`B:dyn LoopState` |
| 被注入于 | — |
<!-- /generated -->
