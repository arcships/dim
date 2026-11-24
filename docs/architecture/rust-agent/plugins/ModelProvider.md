# ModelProvider

Owner: [model_provider.rs](../../../../crates/dim-agent-capabilities/src/model_provider.rs),
[output.rs](../../../../crates/dim-agent-capabilities/src/output.rs) and
[recovery.rs](../../../../crates/dim-agent-capabilities/src/model_provider/recovery.rs).
Runs prepared model calls, validates/commits streamed output and owns terminal
settlement and bounded retry/recovery.

## Image handling

Every conversational request routes image blocks in user content and tool
results by the model's vision capability and the resolved destination's format
support. A vision-capable model, or a request whose capability is unknown,
receives a sendable image as an image block when the destination takes its
format; a model without image input, an image that cannot be sent, or an image
in a format the destination does not take receives a content reference or
omission placeholder. The routing, reference and placeholder rules are owned by
[Attachments A.1](../../../design/plugins/Attachments.md#a1-image-handling-cli-checks-and-modelprovider),
implemented by [images.rs](../../../../crates/dim-agent-capabilities/src/model_provider/images.rs)
using the shared `images::image_reference` helper. The transformation changes
the request only; adopted input, the Timeline and stored tool results never change.
Auxiliary calls do not pass through ModelProvider, so their image pixels stay
intact; a target that does not take an image's format fails the call instead of
turning it into a reference
([ImageUnderstanding §3](../../../design/plugins/ImageUnderstanding.md#3-references)).

The destination's format support belongs to that rule: one whitelist per
protocol, and for the compatible protocol the vendor package the connection's
preset names. Gemini takes no GIF, and every xAI destination takes only PNG and
JPEG — the native routes by their protocol, the built-in `xai` and `grok-build`
entries by the preset their connections carry — so a GIF on Gemini and a WebP or
GIF on an xAI destination become references before the encoder and add nothing
to the budget. The other protocols, and compatible destinations that name no
vendor package or another one, take the four a request may carry.
The provider request encoder applies the destination's message-placement
support: a Bedrock model outside the ones that support images in `toolResult`
receives the tool image in the user message after the result, and the same
families are the only ones AWS supports for `toolResult.status`; the other
Bedrock models receive no such field, and a failed result's text starts with
`[Error: this tool result reports a failed tool call.]`. All of these decisions
stay in the routing rule and the encoder; no catalog field states them.

The adapter selects replacements before copying content. An unchanged request
keeps its Arc without cloning text, thinking, tool arguments or result blocks.
When images change, only those messages are copied; other messages stay shared.

Each image the request sends as pixels joins its 32 MiB encoded-image aggregate
as it is added, so an over-budget request fails during adaptation before the
remaining files are read. An image that becomes a reference after its file is
resolved adds no bytes. The final Aimux boundary reads immutable accepted bytes
without decoding or rescaling them again and checks the aggregate again. A
request permits at most 32 MiB of encoded image data, including user and
tool-result images, and 64 MiB of final HTTP body. Exceeding these limits fails
before sending the request. Each Aimux generation admits four concurrent
request preparations and sends; waiting is cancellable and owner shutdown
releases the waiters. Provider-specific limits still apply.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:ContextPreparation` | Validate budget and access prepared context. |
| `P:ModelHost` | The process model driver; scripted in this local graph. |
| `P:sessionFiles` (optional) | Resolve durable images through the [SessionStore data face](../../../design/appserver/session-files.md#9-interface) before request adaptation. Read at load without a gate; standalone hosts need no file service for text or inline-image requests. |
| `B:Dispatcher` | Replay unsettled calls, receive calls/steers/interrupts and report faults. |
| `B:Timeline` | Commit output. |
| `B:TimelineIdentity` | Session and branch ids of the `model.invoke` span. |

## Events and lifetime

ModelProvider is the private owner of its execution state, tasks and subscriptions;
it publishes `StateCell<ModelProgress>` for observation, not its implementation object.
Calls and interruption enter through the Timeline facts below.

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

A provider's `max_tokens` stop fails that request with a `max_tokens`
presentation, the request's provider/model identity and an action to increase the
output limit. It also writes `failure.kind = output_limit`. The Loop ends that
turn Completed with reason `output_limit`, after its one continuation for calls
the reply announced and never executed, so the stop ends a turn rather than
failing it. It is not a context-budget recovery signal and is never retried
automatically.
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
from the origin, the status, the normalized codes, the `AiMuxError` variant, the
finish reason and the reply's shape, never from message text. A host with no
provider boundary gets `unknown` and the message it already carried. A tool
start whose name is not in `request.tools` fails the request before a call is
committed; it is a host failure, so its `failure.kind` is `unknown`. Arguments
that never complete fail it with `failure.kind = malformed_response`, the kind
the same defect carries when the bridge reports unparseable arguments, unless
the stream reports a content filter after the interrupted call: the known filter
terminal wins and the reply is `content_filtered`.
`failure` is absent on events written before it existed and on failures that are
not a provider call's: the agent loop's unsettled call on a reload and a context
preparation failure. `error` is the default English line
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

The shared invocation path enforces the failure-chain limits before invoking
the model; no separate disposer is required.
ModelProvider sets `failure.retryForbidden` when its failure policy forbids
retrying the failure that ended the request; it is written only when true, and
a continuation declines it.

## Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs):
`invalid_model_stream_is_cancelled_and_drained_before_settlement`,
`transient_retries_are_bounded_and_stop_after_irreversible_output`,
`stop_during_native_drain_keeps_an_already_completed_model_result`,
`vision_and_text_models_route_images_without_changing_history`,
`replacing_a_tool_image_preserves_other_messages_and_the_stored_result`,
`stored_attachment_and_tool_images_route_by_vision_capability`.

<!-- generated:plugin ModelProvider — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`P:ModelHost` |
| 提供 | `B:StateCell<ModelProgress>` |
| 被注入于 | — |
<!-- /generated -->

## Durable images at model execution

During request adaptation, ModelProvider uses `session_files::resolve` to read
a file-backed image for the request metadata's Session, then keeps the bytes
for a vision-capable model, or hashes and releases them for a model that needs
a reference. The resolved source is checked by the same sendable rule, so an
empty owned file takes the placeholder instead of being sent as an empty image.
The same path handles user content and tool results for both kinds of model;
only images in effective context are read. A reference route retains only the
reference; the vision route retains the resolved bytes. The stored request and
its File sources remain unchanged.

The optional dependency is named through `Names` and read with `get_at` at
load. It does not gate or create a reload relation. A missing file service,
Session identity or stored image fails a request that needs file bytes, rather
than inventing a reference or silently omitting it. Cancellation propagates to
bounded reads with the existing per-file storage limit. The aggregate image-data
budget applies during adaptation, as each image the request sends as pixels
joins the request: a reference route carries no image data, so retained images
totaling more than 32 MiB do not block text calls, while a vision request that
carries them as pixels fails during adaptation before the remaining files are
read. Aimux's image-consuming boundary checks the materialized request again.

SessionStore provides `sessionFiles` in the process, under both its shared
dispatch name and the native binding. The native-storage profile also gates
Aimux on that owner, so it is available before ModelProvider's required model
host. Aimux retains its final materialization boundary for direct and auxiliary
image requests; those requests are not converted to conversational references,
and an auxiliary image the target does not take fails the call.
