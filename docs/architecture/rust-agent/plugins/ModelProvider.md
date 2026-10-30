# ModelProvider

Owner: [model_provider.rs](../../../../crates/dim-agent-capabilities/src/model_provider.rs),
[output.rs](../../../../crates/dim-agent-capabilities/src/output.rs) and
[recovery.rs](../../../../crates/dim-agent-capabilities/src/model_provider/recovery.rs).
Runs prepared model calls, validates/commits streamed output and owns terminal
settlement and bounded retry/recovery.

## Image handling

Every image block in a request is handled when the request is built, from that
request's model (Attachments A.1; [images.rs](../../../../crates/dim-agent-capabilities/src/model_provider/images.rs)).
The rule covers images in user content and images inside a tool result alike: a
model that does not accept image input gets the text placeholder in the block's
place, an image the request cannot carry gets the other placeholder, and every
other image is sent as is. The decision changes the request only; the adopted
input, the Timeline and the stored tool result never change.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:ContextPreparation` | Validate budget and access prepared context. |
| `P:ModelHost` | The process model driver; scripted in this local graph. |
| `B:Dispatcher` | Replay unsettled calls, receive calls/steers/interrupts and report faults. |
| `B:Timeline` | Commit output. |
| `B:TimelineIdentity` | Session and branch ids of the `model.invoke` span. |

## Events and lifetime

Consumes `model/call`, `stop/interrupt`, `step/model-start`, `input/applied`,
`turn/start`, `turn/end`. Produces `step/text`, `step/thinking`,
`step/tool-use-start`, `step/tool-use`, `step/model-content`, `model/usage`
and `step/model-output-end`; see Output for block commit boundaries.
The first event of each text/thinking block is preassigned its block id as the
event `id`; later same-kind deltas are grouped by the open block until a
thinking/text/tool event starts the next one. Provider `TextEnd`/`ThinkingEnd`
are not durable boundaries. `ModelProgress` exposes the same ids as
`text_part_id` / `thinking_part_id` while the block is open.
Progress state is transient. Only one call runs at a time. Steer may replace a
request before irreversible output; stop cancels and drains native work.
Cleanup cancels, joins tasks and aggregates failures. Replay fails unended
historical calls without invoking the model again. Retry is bounded and cannot
repeat irreversible output. The process-model constructor also injects the
process `ProviderConnectionResolver` to resolve each request's connection; this
profile does not.

A provider's `max_tokens` stop fails that turn with a `max_tokens` presentation,
the request's provider/model identity and an action to increase the output limit.
It is not a context-budget recovery signal and is never retried automatically.
The private daemon log records model request failures with their Session, turn and request
identities. The Session remains available for a later input or model configuration.
A presentation supplied by the model host takes precedence; ModelFailurePolicy
provides a presentation only when the host supplied none.

## Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs):
`invalid_model_stream_is_cancelled_and_drained_before_settlement`,
`transient_retries_are_bounded_and_stop_after_irreversible_output`,
`stop_during_native_drain_keeps_an_already_completed_model_result`.

<!-- generated:plugin ModelProvider — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`P:ModelHost` |
| 提供 | `B:ModelProvider`、`B:StateCell<ModelProgress>` |
| 被注入于 | — |
<!-- /generated -->
