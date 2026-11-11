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

The adapter selects replacements before copying content. An unchanged request
keeps its Arc without cloning text, thinking, tool arguments or result blocks.
When images change, only those messages are copied; other messages stay shared.

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
`text_part_id` / `thinking_part_id` while the block is open, and the open
thinking block's `thinking_started_at` is the Unix-millisecond time at which it
opened. The block's first durable `step/thinking` event carries that same time
as `create_at`, so the start survives history, replay and fork.
Progress state is transient. Only one call runs at a time. Steer may replace a
request before irreversible output; stop cancels and drains native work.
Cleanup cancels, joins tasks and aggregates failures. Recovery fails unended
historical calls without invoking the model again. It reads only operation and
provider/model identities and failure-chain counters for those calls, plus their tool-use identities and
argument completion state. Both complete and incomplete tool-use arguments are
settled with their existing distinct error texts. Request messages, tools and
system content are not restored. Steer and interrupt state use the same fixed
committed boundary as the unsettled-call query. Retry is bounded by kind:
`network`, `rate_limited`, `server_error` and once `malformed_response` back off
1 s doubling to a 30 s cap within a 300 s budget, over at most 8 attempts, and
it cannot repeat irreversible output. While a retry sleeps, `ModelProgress.retry`
shows the pending attempt; that is display state, not a Timeline fact. The process-model constructor also injects the
process `ProviderConnectionResolver` to resolve each request's connection; this
profile does not.

A provider's `max_tokens` stop fails that turn with a `max_tokens` presentation,
the request's provider/model identity and an action to increase the output limit.
It also writes `failure.kind = output_limit`.
It is not a context-budget recovery signal and is never retried automatically.
The private daemon log records model request failures with their Session, turn and request
identities. The Session remains available for a later input or model configuration.
A presentation supplied by the model host takes precedence; ModelFailurePolicy
provides a presentation only when the host supplied none.

## Failures

A failed call's terminal `step/model-output-end` carries `failure`: the closed
`ModelFailureKind`, the HTTP status and provider code when a response arrived,
the provider's one-line `reason`, the `cause` chain, the attempt count, the
total waited time, and the provider's wait when it asked for more than the
budget left. The bridge
([stream.rs](../../../../crates/dim-agent-aimux/src/stream.rs)) decides the kind
from the origin, the status, the normalized codes and the `AiMuxError` variant,
never from message text. A host with no provider boundary gets `unknown` and the
message it already carried. A tool start whose name is not in `request.tools`
fails the request before a call is committed; it is a host failure, so its
`failure.kind` is `unknown`. `failure` is absent on events written before it
existed and on failures that are not a provider call's: the agent loop's
unsettled call on a reload and a context preparation failure. `error` is the
default English line
`<meaning>: <reason>. <action>`, and the turn that the failure ends repeats the
same string. A `presentation` from the host or from `ModelFailurePolicy` wins
over the client's kind table; it is an override, not the default form. The full
contract is [Model failures](../../../design/model-failures.md).

## Failure chain

A continuation request continues one failure chain. Its `context/prepare` and
`model/call` carry `failure_chain {from_request_id, attempts, waited_ms}`: the
failed request it continues, the attempts that request spent and the backoff it
slept. ModelProvider seeds the new request's retry state with those values, so
the continuation's first call is the chain's next attempt and the chain keeps
one budget of 8 calls and 300 s of sleep. When no attempt or wait budget
remains, the request settles failed without a call. A request without
`failure_chain` gets a fresh budget; a normal request after a tool step is never
part of a chain.

A chain-carrying request's terminal `failure` reports the chain totals, so
`Tried N times over M s` covers the whole chain. That holds for a provider
failure, for the stream's refusal of an undeclared tool start settled through
`invoke`'s `map_err`, for a failure before or outside the invocation, and on a
reload. A continuation whose `context/prepare` fails never reaches ModelProvider:
its `step/model-output-end` carries no `failure`, and the turn ends Failed. Only
the attempt count and the wait budget are shared: the backoff curve and the one
`malformed_response` retry are request-local and restart in the continuation.

The 8 calls and 300 s are ModelProvider's documented public limits: a disposer
reads them and declines a spent chain without reading ModelProvider's state.
ModelProvider sets `failure.retryForbidden` when its failure policy forbids
retrying the failure that ended the request; it is written only when true, and
a continuation declines it.

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
