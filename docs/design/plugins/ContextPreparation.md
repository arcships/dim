# ContextPreparation

Scope: branch. Issue: #2027.

ContextPreparation turns a step's source data into the model request ModelProvider runs, and owns the context budget and the compaction step. The policy details are in [ContextCompaction](ContextCompaction.md); the current implementation is described in the [ContextPreparation plugin page](../../architecture/rust-agent/plugins/ContextPreparation.md).

## Use and scope: responsibility split

ContextPreparation owns:

- **The prepared request.** The request's system, messages and tools at the step's fixed cutoff, from the source data Loop passes and the projection Loop renders at that cutoff.
- **The projection.** The history that becomes the request, rendered by Loop through ContextProjection at the same cutoff and consumed inside the preparation; it crosses once, as a value, and is never persisted.
- **The budget.** The input threshold, the output allowance, the retention reserve and the projection's growth check; an estimate above the threshold starts a compaction before any model call.
- **The compaction step.** The manual command, its single-flight slot and the threshold trigger; the step writes `context/compaction-start` and one `context/compaction`, and Loop starts no request while a step runs.
- **Preparation recovery.** After a reload, a `context/prepare` without a matching `model/call` or `model-output-end`, and a compaction start without a result, settle as terminal facts without redoing host work.
- **The digest entry.** The `dim-digest` guide entry, registered while a strategy is bound.

Callers keep as business:

- Which request runs and when: Loop's turn and step order.
- The adopted configuration and its publication: Prompt.
- The numbers and their calibration: ContextUsage.
- The model settings before adoption: RequestSettings.
- The model call and its settlement: ModelProvider and Provider.
- When the operator asks for a compaction: the client calls `compact`.

| Requirement (caller) | Can callers compose it from the operations below without breaking ContextPreparation's invariants? | Handling |
| --- | --- | --- |
| Loop starts a step's request at a cutoff | No: the request's content, budget and handoff facts are the owner's; the projection crosses once as a value at the same cutoff | `prepare` with the source data and the projection |
| ModelProvider confirms the final request fits the budget before sending it | No: the estimate reuse and the counter are the owner's, and a caller cannot know which estimate still matches | `validate` |
| The operator asks for a manual compaction and needs its outcome | No: the single-flight command, the step and its outcome are one state machine | `compact` |
| A caller reads whether a compaction is running | Yes: `context/compaction-start` and `context/compaction` are public facts | Business composition with `timelineTree.read` |
| A caller reads the current usage numbers | Not this owner: ContextUsage owns the numbers | ContextUsage's `read` |

An operation exists only where the row says No.

## Caller example

Loop prepares a step at its cutoff; the projection is rendered once and handed over as a value:

```rust
let projection = context_projection
    .rendered_stream(RenderedStreamRequest { through: cutoff, batch: 256 })
    .await?;
context_preparation
    .prepare(
        PrepareRequest {
            source: ContextPrepareData { turn_id, request_id, through: cutoff, ..source },
            projection,
        },
        cancel.clone(),
    )
    .await?;
// No request object crosses: the owner writes model/call, and Loop consumes that fact in order.
```

ModelProvider validates the final request, after image adaptation:

```rust
context_preparation
    .validate(ValidateRequest { request }, cancel.clone())
    .await
    .map_err(|error| match error {
        PreparationError::OverBudget { .. } => HostError::budget(),
        other => HostError::from(other),
    })?;
```

The operator asks for a manual compaction; the reply is the step's outcome and the caller's cancel stops only the wait:

```rust
let outcome = line_owner::<ContextPreparation>(line)?
    .compact(CompactionRequest {}, cancel.clone())
    .await?;
match outcome {
    CompactionOutcome::Completed { sequence } => reply_done(sequence),
    CompactionOutcome::Skipped { reason } => reply_skipped(reason),
    CompactionOutcome::Failed { reason } => reply_failed(reason),
}
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| ContextProjection (#2025, branch) | `renderedStream`, `renderedContext`, `snapshot`, `streamCheckpoint`, `snapshotFromCheckpoint` | The request's history, the compaction's cut and the recovery snapshot. |
| ContextFacts (#2025, Session) | `describe`, `count`, `contributions`, `estimate` | The model's limits and the request's tokens. |
| SummaryStrategy (#2025, Session) | `describe`, `compact` | The replacement's planned size and the compaction itself. |
| ContextUsage | `estimate`, `confirm`, `read` | The budget baseline and the saved contributions. |
| GuideEntries (#2026) | `register` | The `dim-digest` entry while a strategy is bound. |
| BranchScope | `timeline`, `dispatcher`, `timelineIdentity`, `branchLifecycle` | The line's facts, order, identity and phase. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `prepare` | Admit a step's request, build it at its fixed cutoff and publish the handoff facts | `{source, projection}` | `{}` once `model/call` or the failure terminal is committed | The caller's cancel before admission writes nothing; after admission the owner settles the request | `invalid` (malformed source data or a projection from another cutoff), `cancelled`, `closed` |
| `validate` | Confirm the final request fits the budget, reusing the estimate when it still matches | `{request}` | `{}` | The caller's cancel stops the check; nothing changes | `over_budget` (the request is too large; the caller fails the turn or recovers), `invalid` (missing or foreign capability snapshot), `cancelled`, `closed` |
| `compact` | Run one manual compaction step and answer its outcome | `{}` | `{outcome}`: `completed{sequence}`, `skipped{reason}`, `failed{reason}` or `cancelled{reason}` | The command is written before the first await; the caller's cancel or a dropped wait stops only the wait, and a later call joins the committed step | `cancelled` (the wait stopped; the step continues), `closed` |

Loop writes `context/prepare` before the call. `prepare` then writes `model/call`, or, when the host or the budget refuses before ModelProvider is reached, the `step/model-output-end` terminal as the stated exception in [ModelProvider](ModelProvider.md). The projection is consumed inside the call and never persisted; the request is the durable form, and no prepared object crosses. `validate`'s reuse is expressed by the request data and the preparation's own record, never by object identity.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `invalid` | `prepare`, `validate` | Fix the input: malformed source data, a projection from another cutoff, a capability snapshot that belongs to another model |
| `over_budget` | `validate` | The request is over the input threshold; end the turn or run the compaction path, then build a new request |
| `cancelled` | `prepare`, `validate`, `compact` | Nothing was committed by the cancelled call; a committed compaction continues and answers `cancelled{reason}` |
| `closed` | all | The line is closing; stop preparing requests; the next generation serves a reloaded line |

`compact` reports a failed summary as the `failed` outcome, not an error: the compaction fact carries the same diagnostics either way.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `prepare` | Take the single-flight slot, apply the budget to the projection, build the request, write `model/call` (or its failure terminal) | Before admission: nothing written. After admission: the preparation task finishes or ends cancelled; a steer waits for a running compaction | New preparations are refused; in-flight ones settle with `model-output-end`; an unconsumed handoff is discarded; recovery settles the facts without host work | ContextPreparation; the request's external effect by ModelProvider and Provider |
| `validate` | Reuse the matching estimate or count the final request; answer, or fail the request | The check stops; nothing changes | In-flight checks end `closed` | ContextPreparation |
| `compact` | Join or write the command, run the step at the boundary or immediately, write `context/compaction-start` and the outcome | The wait stops; the committed step continues and its result is read by a later call | An in-flight step is cancelled and settled `cancelled`; recovery reports a start without a result | ContextPreparation; the summary model calls by SummaryStrategy and Provider |
| digest entry | Register on `apply`, withdraw on unload | — | The entry leaves with the part; a reloaded part registers the new text | ContextPreparation |

## Exceptions with reasons

- The threshold compaction runs inside `prepare`, not as a separate operation: an over-threshold request must either fit after the step or fail as that request, and a caller composing the two calls would have to hold the request and the slot across them.

## Supersedes

- `docs/design/contracts/runtime-owner-operations.md`: the `contextPreparation` `publish`/`awaitPublished` row and the `agentLoop.compact` row; the Loop and ContextPreparation host-port rows keep their owners (#2025) and this contract's callers.
- `docs/design/service-faces.md` section 3.5: the `contextPreparation` and `configurationPublisher` rows.
- [ContextCompaction](ContextCompaction.md) section 1 items 4-5, section 1.1, section 2, section 2.1, section 2.2, section 3, section 4.1, section 4.3, section 5.3-5.5, section 5.8, section 10 and section 12: the command, the single-flight slot and the step are this owner's, and the Loop starts no request while a step runs.
- [Prompt](Prompt.md) section 6.3: the published System is Prompt adoption's, and ContextPreparation reads the adopted fact.

## Verification

- A prepared request equals the request built from the same source data and cutoff; the projection is rendered once, crosses once, and is not persisted.
- An over-threshold estimate runs the compaction inside `prepare`; a compaction that cannot fit the request writes `model-output-end` and no recovery request follows.
- `validate` reuses the estimate for the unchanged final request and counts the request otherwise; a request over the threshold is `over_budget`.
- A cancel during `prepare` before admission writes nothing; a cancel during `compact` stops the wait and the step settles later.
- A reload settles an unmatched `context/prepare` and a compaction without a result as terminal facts and performs no host work.
- The `dim-digest` entry exists only while a strategy is bound.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds the line's facts and writes prepare and compaction facts. |
| §2 scopes | Branch: the request and its compaction belong to one line. |
| §3 orthogonality | No other owner's state is read; the model facts and the summary come from Session owners' operations. |
| §4 rule 1 | A manual compaction is the `context/compact` command; the outcome is the owner's fact. |
| §4 rule 2 | The request's handover is `model/call`; after it ModelProvider owns the call. |
| §4 rule 3 | Handlers register tasks; host preparation runs in those tasks. |
| §4 rule 4 | No business meaning: what a request is for stays with Loop. |
| §4 rule 5 | A compaction start and its outcome commit as the owner decides; the request's facts keep their order. |
| §4 rule 6 | The context is the projection; nothing enters outside the prepared request. |
| §4 rule 7 | Callers use the three operations; the owner uses the siblings' operations. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Compaction facts are durable history; a recovery snapshot is optional and never replaces them. |
| §7 values, ids and objects | The request and the projection cross as data; no object, handle or callback crosses. |
| §7 one interface | One name, `contextPreparation`, on the owner type; `configurationPublisher`, `usage` and the crate-private projection slot are removed. |
| §7 cross-language | Every operation is data; no method exists only for Rust. |
| ADR 002 D1 | Branch: preparation and compaction are per-line work. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- #2025 (ContextProjection): `renderedStream` is Loop's call; the projection crosses to `prepare` as a value at the same cutoff, as the ContextProjection contract states.
- #2025 (PrepareContextHost): the model profile's request builder remains the host port `prepare` calls; the owner contract does not replace it.
- #2025 (ContextFacts, SummaryStrategy): the counting and summary operations above replace `contextModelHost` and `compactionStrategyHost`; this contract uses only those operations.
- #2026 (GuideEntries): `register` must accept a line-scoped entry and withdraw it with the part.
- #2025 (Session access): the client's `session.context.compact` reaches `line_owner::<ContextPreparation>(line)?.compact(..)` by line id; ADR 027's id-to-instance step resolves the branch group's data face.

## Binding appendix

The service is marked on the `ContextPreparation` type; the wire name stays `contextPreparation`.

```rust
use crate::host::CancelledFn;
use dim_agent_types::{ContextPrepareData, ModelRequest};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareRequest {
    pub source: ContextPrepareData,
    pub projection: crate::history::StreamedContext,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateRequest {
    pub request: ModelRequest,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionRequest {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum CompactionOutcome {
    Completed { sequence: u64 },
    Skipped { reason: String },
    Failed { reason: String },
    Cancelled { reason: String },
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PreparationError {
    #[error("invalid: {message}")]
    Invalid { message: String },
    #[error("over budget")]
    OverBudget { estimated_input_tokens: f64, threshold_tokens: f64 },
    #[error("cancelled")]
    Cancelled,
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl ContextPreparation {
    pub async fn prepare(&self, request: PrepareRequest, cancelled: CancelledFn) -> Result<(), PreparationError> { /* .. */ }
    pub async fn validate(&self, request: ValidateRequest, cancelled: CancelledFn) -> Result<(), PreparationError> { /* .. */ }
    pub async fn compact(&self, request: CompactionRequest, cancelled: CancelledFn) -> Result<CompactionOutcome, PreparationError> { /* .. */ }
}
```
