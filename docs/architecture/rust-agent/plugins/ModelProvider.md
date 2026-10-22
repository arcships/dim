# ModelProvider

Owner: [model_provider.rs](../../../../crates/dim-agent-capabilities/src/model_provider.rs),
[output.rs](../../../../crates/dim-agent-capabilities/src/output.rs) and
[recovery.rs](../../../../crates/dim-agent-capabilities/src/model_provider/recovery.rs).
Runs prepared model calls, validates/commits streamed output and owns terminal
settlement and bounded retry/recovery.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:ContextPreparation` | Validate budget and access prepared context. |
| `S:ModelHost` | Scripted model driver in this local graph. |
| `B:Dispatcher` | Receive calls/steers/interrupts and report faults. |
| `B:Timeline` | Commit output and reconstruct unsettled calls. |
| `B:TimelineIdentity` | Session and branch ids of the `model.invoke` span. |

## Events and lifetime

Consumes `model/call`, `stop/interrupt`, `step/model-start`, `input/applied`,
`turn/start`, `turn/end`. Produces `step/text`, `step/thinking`,
`step/tool-use-start`, `step/tool-use`, `step/model-content`, `model/usage`
and `step/model-output-end`; see Output for block commit boundaries.
Progress state is transient. Only one call runs at a time. Steer may replace a
request before irreversible output; stop cancels and drains native work.
Cleanup cancels, joins tasks and aggregates failures. Replay fails unended
historical calls without invoking the model again. Retry is bounded and cannot
repeat irreversible output. The alternate process-model constructor enables
process ModelHost/resolver dependencies; those are not injected in this profile.

## Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs):
`invalid_model_stream_is_cancelled_and_drained_before_settlement`,
`transient_retries_are_bounded_and_stop_after_irreversible_output`,
`stop_during_native_drain_keeps_an_already_completed_model_result`.

<!-- generated:plugin ModelProvider — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:ModelHost` |
| 提供 | `B:ModelProvider`、`B:StateCell<ModelProgress>` |
| 被注入于 | — |
<!-- /generated -->
