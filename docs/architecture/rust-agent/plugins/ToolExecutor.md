# ToolExecutor

Owner: [tool_executor.rs](../../../../crates/dim-agent-capabilities/src/tool_executor.rs).
Executes a committed tool batch against pinned handlers after approval, isolates
individual call failures and publishes correlated results.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:Approval` | Admit each call under permission/approval policy. |
| `B:ToolBinding` | Resolve the request's pinned tools. |
| `B:TimelineIdentity` | Construct the calling branch execution context; Session and branch ids of the `tool.execute` span. |
| `B:Dispatcher` | Receive batches/interrupts and report faults. |
| `B:Timeline` | Replay batches and commit progress/results. |

## Events and lifetime

Consumes `step/tool-call`, `stop/interrupt`, `turn/start`, `turn/end`; produces
`step/tool-start`, `step/tool-completed` when progress is enabled and
`step/tool-result`. Calls in a batch settle before the correlated batch result.
Only one batch runs at once. Interrupts cancel current work and suppress late
host invocation. Cleanup closes admission, cancels, joins tracked work and
aggregates failures. Reload settles unmatched committed batches as errors rather
than rerunning effects; reload-settled results make Loop fail the turn.
Cancellation is not rollback of a tool's completed side effect.

## Tests

[interrupt_flow.rs](../../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs):
`a_tool_call_already_committed_when_the_interrupt_arrives_is_not_executed`;
[review_flows.rs](../../../../crates/dim-agent-capabilities/tests/review_flows.rs):
`one_tool_panic_does_not_abandon_other_native_cleanup_or_reorder_actual_results`.

<!-- generated:plugin ToolExecutor — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Approval`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding` |
| 提供 | `B:ToolExecutor` |
| 被注入于 | — |
<!-- /generated -->
