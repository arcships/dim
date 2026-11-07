# Model failures

What a failed model request reports, and how the retry policy reads it (issue
#1197).

## 1. One closed kind, decided where the provider error is read

The aimux bridge is the only place that holds the provider's structured error,
so it decides `ModelFailureKind`. It uses only the origin, the HTTP status, the
normalized codes and the `AiMuxError` variant. It never matches message text.

| kind | decided from | retried | meaning | action |
| --- | --- | --- | --- | --- |
| `network` | `ApiCall`, origin Transport, no HTTP status | yes | could not reach the provider, or the connection dropped | check the network or proxy |
| `rate_limited` | 429, rate-limit codes | yes; `Retry-After` is used while it fits the wait budget | the provider is rate limiting requests | wait and try again |
| `server_error` | 408, 5xx except 501 and 505, server and overload codes, transient stream-event codes | yes | the provider reported a server error | try again later or choose another model |
| `malformed_response` | `InvalidResponseData`, `JsonParse`, `InvalidToolInput`, `NoSuchTool`, `TypeValidation`, an undecodable stream | once | the provider sent a response Dim could not use | try again; if it repeats, choose another model or check the proxy or gateway |
| `auth_rejected` | 401, 403, invalid-key/auth codes | no (the existing one credential recovery stays) | the provider rejected the credentials | sign in again or check the API key |
| `quota_exhausted` | 402, quota/billing codes | no | the account has no quota left | check the plan or billing |
| `not_found` | 404 | no | the provider did not find the model or endpoint | check the model and the provider's base URL |
| `context_too_long` | 413, context-length codes | no | the conversation is longer than the model accepts | compact the conversation or start a new session |
| `content_filtered` | content-policy codes | no | the provider's content filter blocked the response | rephrase the request |
| `invalid_request` | 400, 409, 422, 501, 505, other 4xx, invalid-request codes including `max_tokens_exceeded`, `InvalidArgument`, `InvalidPrompt`, `UnsupportedFunctionality` | no | the provider rejected the request | see the details |
| `output_limit` | the `max_tokens` stop | no | the model stopped at its output limit | increase the output token limit and retry |
| `unknown` | anything else | no | the model request failed | see the details |

A timeout of the HTTP connection or of the stream reaches the bridge as a
transport `ApiCall` and is `network`. `AiMuxError::Timeout` has no producer in a
chat call; if it ever reaches the bridge it is `unknown` and is not retried.

Codes are compared trimmed and ASCII-lowercased.

- 409, 501 and 505 are `invalid_request` whatever transient code came with
  them; a permanent code still decides its own kind.
- A stream-event failure carries the response's status, not the event's, so only
  its code may decide: a transient code maps to `rate_limited` or
  `server_error`, a permanent code to its kind, anything else to `unknown`.

Every `HostError::Failed` the bridge produces carries facts, including
`AiMuxError::Timeout`, `InvalidResponseData`/`JsonParse`, `InvalidArgument`/
`InvalidPrompt` and `UnsupportedFunctionality`.

The kind is only as good as the provider's structured fields. A provider that
reports, say, quota as a bare 429 with only a message becomes `rate_limited` and
is retried uselessly within the budget; the `reason` line still shows the
provider's own message. A missing code is added to the bridge's code list, never
matched from text.

## 2. The fact

`ModelOutputOutcome::Failed` gains `failure: Option<ModelFailure>`:

```
ModelFailure {
  kind,             // section 1
  status?,          // HTTP status, when a response arrived
  providerCode?,
  requestId?,
  reason?,          // one specific sentence, one line, at most 300 chars
  cause: string[],  // the chain, outermost first; each one line, at most 200 chars, at most 6 frames
  attempts,         // at least 1
  waitedMs,         // total backoff slept
  retryForbidden?,  // true only when the failure policy forbids retrying
  retryAfterMs?     // the provider asked to wait longer than the budget left, so Dim stopped
}
```

- `failure` is optional. It is absent on events written before it existed, and
  on failures that are not a model provider call's: the agent loop's
  unsettled-call failure on a reload (`agent_loop.rs`) and a context preparation
  failure (`context_preparation.rs`). The field follows `presentation`'s casing
  and `optional_non_null` handling.
- `retryForbidden` is present only when true. ModelProvider sets it when its
  failure policy forbids retrying the failure that ended the request, and a
  continuation declines it, so the prohibition covers the continuation.
- `reason` is the provider's error message for a response or stream-event
  failure, and the innermost frame of the cause chain for a transport failure.
  A JSON parse message embeds the body text it could not read, so its `reason`
  is the parse diagnosis from the error's structure, and is absent when the
  error carries no cause. A tool failure is named from its `tool_name` field:
  `invalid arguments for tool '<tool_name>'` or `the model called an unknown
  tool '<tool_name>'`. `TypeValidation` carries no reason, because its message
  embeds the validated value. The tool input, the available-tool list and the
  validated value are never stored.
- `cause` is the chain from `ApiCallError.message` and `ErrorCause`, outermost
  first, one line per frame. `providerCode` and `requestId` are one bounded
  line, at most 200 chars; an empty one is absent.
- Never stored: the request URL, headers, request values, the response body or
  any credential. aimux already drops the URL; the bridge must not copy `url`,
  `request_body_values` or `response_body`, and `reason` and the cause frames
  are text only, with control characters stripped.
- `error`, which old clients read, is the default English line
  `<meaning>: <reason>. <action>`, with `: <reason>` left out when there is
  none. There is no `ApiCall:` prefix. The turn that the failure ends carries
  the same string in `TurnOutcome::Failed.error`, so a client can tell the two
  apart and drop the repeat.
- The `max_tokens` stop keeps its presentation and also writes
  `failure.kind = output_limit`.
- A failure from a host with no provider boundary gets kind `unknown` and the
  message it already carried.

## 3. Who words the line

Clients word their own line from `kind`, through their own i18n. The fact
carries the kind and the data, not a sentence. `presentation` keeps its meaning:
wording that a host or a `ModelFailurePolicy` supplies wins over the client's
kind table. It is an override, not the always-present form.

The terminal fact's `error` is a complete fallback line, so every renderer that
already prints `error` shows the better line without a change of its own.

A failed request shows one line: `<meaning>: <reason>. <action>`, with
`: <reason>` left out when the fact carries none. When the fact records more
than one attempt, the line ends with `Tried N times over M s.`, the total wait
rounded to whole seconds; a single attempt adds nothing. The details keep their
own `tried N times over M s`. The words come from the kind, and the line takes
no `model failed:` prefix. A request that already showed text, thinking or a
tool row starts the line with "The response above is incomplete.", which each
client derives from the rows it holds rather than storing. An event written
before the fact existed keeps the `error` line and its prefix.

The turn's own `failed` outcome does not repeat the model failure: an error equal
to the one the turn's model failure already showed prints nothing. The
suppression is per turn: every turn end forgets what a previous turn showed,
whether that turn completed, failed or was cancelled, so a later turn's failure
with the same text still prints. A turn failure that is not a model failure
still prints.

The CLI line mode and `exec` print that one line. The TUI and the Web keep its
details one step away: the kind, the HTTP status, the provider code, the request
id, `tried N times over M s` when there was more than one attempt, the wait the
provider asked for, and the cause chain, one frame per line. The TUI details join
the `/tools` cycle and the click-to-expand of an error row; the Web opens them
from a "Details" disclosure under the row. Both are collapsed by default.

While a request waits to retry, the TUI's working line and the Web's working
indicator read `Retrying 3/8 in 4 s · <meaning>: <reason> · <stop>`, counting
down from `ModelProgress.retry.retryAtMs` on the client clock. Once that time
passes before new progress arrives, they say `Waiting for the provider` with
the kind's meaning instead of `in 0 s`. The visible countdown is not a live
region: a screen reader hears `Retrying, attempt 3 of 8: <meaning>` when a wait
starts or the attempt changes, never on each tick. The CLI line mode writes one
line per attempt where it writes its other status lines; `exec` writes none. A
retry adds no transcript row, and a later success adds none either; the failure
line's `tried N times` is the persistent record.

## 4. Retry

`invoke` is shared by ModelProvider, the auxiliary model and the summary
strategy, so all three retry the same way; there is one implementation and no
per-caller setting.

| item | value |
| --- | --- |
| retried kinds | `network`, `rate_limited`, `server_error`, `malformed_response` |
| attempts | at most 8 in total per request, the first included |
| `malformed_response` | retried once per request: a second malformed response stops, whatever came before it |
| backoff | the n-th retry sleeps 1 s × 2^(n−1), capped at 30 s, times a uniform jitter in [0.8, 1.0] |
| `Retry-After` | used exactly as the delay, even above 30 s, while it fits in the budget left; if it does not fit, the request stops at once and records `retryAfterMs` |
| wait budget | a request sleeps at most five minutes in all before it fails; a wait that does not fit in what is left stops the request at once |
| unchanged | no retry after irreversible output; an interrupt cancels the sleep; one credential recovery with no delay, still within the 8 attempts; `ModelFailurePolicy::forbids_retry`; a terminal credential rejection is settled |

The nominal backoff sleeps are 1, 2, 4, 8, 16, 30 and 30 s, about 91 s in all.
The numbers follow the outage in #1197, which lasted tens of seconds: eight
attempts ride out about 90 s, a 1 s base avoids sub-second attempts that hit
the same outage, the 30 s cap keeps the last attempts from being minutes
apart, and a server's own advice sets the timing without being able to extend
the budget. A request can wait up to five minutes in all before it fails;
recovery of a rejected credential never waits, whatever advice it carried.

While it sleeps, the wait is published as `ModelProgress.retry`: `attempt` (the
attempt about to run, at least 2), `maxAttempts`, `retryAtMs` (unix
milliseconds at which it starts), `kind` and the failure's bounded `reason`.
`retryAtMs` is wall-clock time for display and is advisory; the budget is
measured on a monotonic clock, so a client must not treat it as a deadline.
ModelProvider sets the field before the sleep and clears it when the attempt
starts, the call ends, or the sleep is cancelled. It is not a Timeline fact: no
later decision reads it, and the terminal fact keeps `attempts` and
`waitedMs`.

### The failure chain

A continuation request continues the request that failed before it. Its
`context/prepare` and `model/call` carry
`failure_chain {from_request_id, attempts, waited_ms}`: the failed request, the
attempts it spent and the backoff it slept. ModelProvider seeds the
continuation's retry state from them, so its first call is the chain's next
attempt and the chain keeps one budget of 8 calls and 300 s of sleep. With no
attempt or wait budget left, the request settles failed without a call. A
chain-carrying request's terminal `failure` reports the chain totals, so the
failed line's `Tried N times over M s` covers the whole chain. That holds for a
provider failure, for the stream's refusal of an undeclared tool start settled
through `invoke`'s `map_err`, for a failure before or outside the invocation,
and on a reload. A continuation whose `context/prepare` fails never reaches
ModelProvider: its `step/model-output-end` carries no `failure`, and the turn
ends Failed. Only the attempt count and the wait budget are shared: the backoff
curve and the one `malformed_response` retry are request-local and restart in
the continuation. A request without the field gets a fresh budget; a normal
request after a tool step is never part of a chain.

The 8 attempts and the 300 s are ModelProvider's public limits: a disposer
reads them and declines a spent chain without reading ModelProvider's state.
ModelProvider sets `failure.retryForbidden` when its failure policy forbids
retrying the failure that ended the request; the field is written only when
true, and a continuation declines it.

## 5. Compatibility

The fields are additive. A binary without `failure` cannot decode an event
written with it, because `ModelOutputOutcome` denies unknown fields. That is the
same forward-compatibility limit `presentation` has, so a downgrade across this
change is unsupported, as it already was for `presentation`.

The same decision covers the continuation fields, the retry prohibition and
the new kind. The optional `failure_chain` on `ContextPrepareData` and
`ModelCallData`, the optional `handover` on `InputMessageData`, and the optional
`retryForbidden` on `ModelFailure` are additive, and a binary without them
cannot read a record that carries them, so a downgrade across this change is
unsupported. The new `step/disposition` kind is additive for the native store
and absent from v1: the outbound boundary filters it like `input/consumed`, so
the frozen v1 notification set is unchanged. Each addition registers the
decision in the contract tests with its compatibility cases; the frozen
fixtures stay unchanged.
