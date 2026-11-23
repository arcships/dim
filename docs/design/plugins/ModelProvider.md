# ModelProvider

Scope: branch. Issue: #2027.

ModelProvider owns one line's model call: it consumes `model/call`, runs the request through Provider, commits the streamed output to the line's Timeline, and settles one terminal fact. Provider owns the model, the credential, retries and the transport; the current implementation is described in the [ModelProvider plugin page](../../architecture/rust-agent/plugins/ModelProvider.md).

## Use and scope: responsibility split

ModelProvider owns:

- **The call's lifetime.** The one call a `model/call` fact starts, its cancellation, its drain and its generation.
- **Streamed output.** The `step/text`, `step/thinking`, `step/tool-use-start`, `step/tool-use` and `step/model-content` blocks, their ids and their commit boundaries.
- **The terminal fact.** `step/model-output-end` with the outcome, the failure chain totals and the runtime's failure kinds.
- **Durable images.** Before the call, resolving a file-backed image for the Session the request names; the request that reaches Provider carries only what the model may receive.
- **Progress.** The active request's block ids and character counts as a value other owners and clients read.

Callers keep as business:

- Which model runs and with which credential: Provider.
- The request's messages, system and tools: ContextPreparation.
- What a failure means for the turn: Loop decides the turn's end and its one continuation.
- How a client renders the stream: the client.
- When to stop: the operator's stop is a `stop/interrupt` fact; Loop and ModelProvider settle their own parts.

| Requirement (caller) | Can callers compose it from the operations below without breaking ModelProvider's invariants? | Handling |
| --- | --- | --- |
| A client or a Session method reads the active output's length and follows changes | No: the block ids and character counts are the owner's transient state; a caller folding `step/*` events duplicates the stream | `progress`, `observe` |
| Loop learns how the call ended | Yes: the terminal fact | Business composition |
| A client follows the streamed text | Yes: `step/text` facts, ordered by the Timeline | Business composition |
| A caller stops the call | Not this owner: a stop is `stop/interrupt`; Loop writes it and ModelProvider settles | Business |

An operation exists only where the row says No.

## Caller example

A Session method answers the active output of an open turn:

```rust
let Progress { request_id, text_characters, thinking_characters, .. } =
    model_provider.progress()?;
```

A client subscribes to progress for the rest of its connection:

```rust
model_provider
    .observe(&ctx, Arc::new(move |progress| notice(progress)))
    .await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| Provider (#2019, process) | `invoke` | One model request: selection, credential, transport, retries, recovery, streaming and the consumer callback. |
| SessionFiles (#2018, process) | `read` | The bytes of a file-backed image the request references; optional in profiles without the native store. |
| ContextPreparation (branch) | `validate` | The final request's budget check after image adaptation. |
| BranchScope | `timeline`, `dispatcher`, `timelineIdentity` | The line's facts, their order and its identity. |

`Provider::invoke` adapts image references to the model's capability; ModelProvider resolves the durable bytes before the call and never sends a reference it cannot resolve.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `progress` | The active request's output so far | `{}` | `{requestId?, textCharacters, thinkingCharacters, textPartId?, thinkingPartId?, thinkingStartedAt?, retry?}` | None | `closed` |
| `observe` | The current progress, then every change, until the row unloads | the listener argument | `{}` | The row's unload ends the subscription; nothing is released by the caller | `closed` |

The reply is a value: no block, stream or request object crosses. `listener` is a callback by reference, called outside the owner's locks and outside the Timeline's commit lock.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `closed` | `progress`, `observe` | The line is closing; the next generation serves a reloaded line |

`progress` answers an empty value when no request is active; an absent request id is not an error.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `progress`, `observe` | Read the published value, or register the listener under the calling row and call it once, then on each change | The row's unload drops the listener | The generation stops calling and releases every listener | ModelProvider; the listener's work by the calling row |
| the model call | A `model/call` fact starts the call; resolve durable images, validate the final request, call `provider.invoke` with a consumer that writes `step/*` blocks; write one `step/model-output-end` with the outcome and the chain totals | The `stop/interrupt` fact cancels the call and drains Provider's stream; the terminal fact records the reason | An unended historical call settles failed without a model call; accepted output stays committed; no external effect is repeated | ModelProvider: the call, the output blocks and the terminal fact; Provider: the request, its credential and its stream; Loop: the turn |

The owner writes no fact before the batch that carries it commits, and a failed output closes every started call with an error result in the same batch as the terminal, so no call of a failed output reaches a tool.

## Client-visible changes

The `session.model.progress` notice keeps its field names and gains three additive fields: `requestId`, `textCharacters` and `thinkingCharacters`. The additions let `session.turn.get` read the active output from the owner instead of folding `step/text` and `step/thinking` facts; a client that ignores unknown fields is unaffected. No other client-visible behavior changes: `session.turn.get` and the model-visible texts keep their shapes.

## Exceptions with reasons

- The failure chain and its retry display are Provider's execution data but the terminal fact is ModelProvider's: the fact is the branch's durable record of the call, and Provider has no Timeline.
- A terminal fact for a reload-settled call carries no Provider failure: no provider call ran, so the owner does not invent one.

## Supersedes

None: the contract adds the owner's first public read and keeps the provider and Timeline contracts as they are.

## Verification

- `progress` reports the open blocks and character counts, and is empty when no request runs; `observe` delivers the current value and each change, and stops at the row's unload or the owner's close.
- A `model/call` writes the `step/*` blocks in order and exactly one terminal fact; a failed output's started calls are closed with error results in the terminal's batch.
- A stop cancels the call and drains Provider's stream; an already committed block stays committed and the terminal names the cancellation.
- A reload fails an unended call without invoking Provider again; the request's messages and tools are not reconstructed.
- An unresolvable file-backed image fails the request instead of inventing a reference or silently omitting it.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds the call's facts and writes its own output facts. |
| §2 scopes | Branch: the call and its output belong to one line. Provider is the process owner it calls. |
| §3 orthogonality | No other owner's state is read; the request and the stream come through Provider. |
| §4 rule 1 | No command event of its own; `model/call` and `stop/interrupt` are the commands it consumes. |
| §4 rule 2 | The request handover is `model/call`; after it ModelProvider owns the call. |
| §4 rule 3 | Handlers register tasks; the provider stream runs in those tasks. |
| §4 rule 4 | No business meaning: which model and which credential are Provider's. |
| §4 rule 5 | A terminal fact and its error results commit in one batch. |
| §4 rule 6 | Output enters context only as committed blocks and the terminal result. |
| §4 rule 7 | Callers use the two reads; the owner uses Provider's operation. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Every block and terminal is published after its batch commits. |
| §7 values, ids and objects | The progress is a value; `observe` takes a callback; no stream or request object crosses. |
| §7 one interface | One name, `modelProvider`, on the `ModelProvider` type; the `StateCell<ModelProgress>` publication is removed. |
| §7 cross-language | Every operation is data or a callback. |
| ADR 002 D1 | Branch: one call is bound to a line and a turn. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2019 (Provider): `invoke` must accept the branch consumer callback, the failure chain and the image-adapted request, as the Provider contract states; ModelProvider adds nothing to that request beyond durable image bytes.
- #2018 (SessionFiles): `read` must answer one file-backed image's bytes for the Session named in the request metadata, within the owner's limit.
- #2025 (Session access): `session.turn.get` reads `line_owner::<ModelProvider>(line)?.progress()`; ADR 027's id-to-instance step resolves the branch group's data face.

## Binding appendix

The service is marked on the `ModelProvider` type; the wire name is `modelProvider`.

```rust
use dim_agent_runtime::PublicService;
use rutis::Ctx;
use std::sync::Arc;

pub type ProgressListener = Arc<dyn Fn(Progress) -> rutis::host::BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync>;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub text_characters: u64,
    pub thinking_characters: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_part_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_part_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryProgress>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryProgress {
    pub attempt: u32,
    pub next_attempt_after_ms: u64,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ProgressError {
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl ModelProvider {
    pub fn progress(&self) -> Result<Progress, ProgressError> { /* .. */ }
    pub async fn observe(&self, caller: &Ctx, listener: ProgressListener) -> Result<(), ProgressError> { /* .. */ }
}
```
