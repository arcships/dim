# ContextUsage

Scope: branch. Issue: #2027.

ContextUsage owns the numbers of a line's effective context: what each source contributed, the cumulative totals, the compaction cuts and the provider's measured baseline. The counting rules and the saved formats are decided in [ADR 023](../../adr/023-context-usage.md); the compaction interaction is [ContextCompaction §5](ContextCompaction.md#5-usage-and-the-compaction-cut). This document is the owner's target contract.

## Use and scope: responsibility split

ContextUsage owns:

- **Contributions.** One number per source that actually produced context: an adopted input, a settled assistant output, a tool result, a compaction's replacement and its baseline. The contribution is `{serialized_bytes, images}`; the count is bytes / 4 + images × 1600 under the counter identity `dim/window-facts/v1`.
- **The saved totals.** One `context-usage/recovered` record per changed source and a `context-usage/covered` extension for a checked no-effect tail, at the version the format decides.
- **The request baseline.** The estimate a preparation reserves for a request, the projection it counted, and the provider's measured prompt tokens when they verify that same request.
- **Recovery.** After a reload, the latest saved totals, the covered position and the counter controls. An uncovered source invalidates the derived numbers; a cold recovery never consumes a source body, and a missing predecessor answers unavailable rather than recounting history.

Callers keep as business:

- What the budget decision is: ContextPreparation's threshold and retention rules.
- Which model a request runs and how it is counted: ContextFacts and RequestSettings.
- Whether a saved count may be reused: ContextPreparation asks for the estimate and validates the final request.
- What a usage number means to a client: the client renders it.

| Requirement (caller) | Can callers compose it from the operations below without breaking ContextUsage's invariants? | Handling |
| --- | --- | --- |
| A client or a Session method reads the current usage at a cutoff | No: the totals, the cut and the baseline are one record; folding them from facts in the caller is a second cache | `read` |
| ContextPreparation reserves the input budget of a request | No: the estimate must be tied to the projection it counted and reused only while that projection still matches | `estimate` |
| ContextPreparation confirms the final request that was sent | No: only the owner may save contributions for a request, and only once the request retained the estimate's projection | `confirm` |
| A caller learns why usage is unavailable | Yes: `not_configured` and `not_ready` name the next step | Business |
| A caller recounts history after a compaction | Yes: the compaction's replacement and baseline arrive as facts and the owner folds them | Business |

An operation exists only where the row says No.

## Caller example

A Session method reads the current numbers:

```rust
let usage = context_usage.read(ReadUsage { through: None }, cancelled).await?;
// usage: {through, contextWindow, outputTokens, thresholdTokens, estimatedInputTokens, baselineSource}
```

ContextPreparation reserves a request's budget, then confirms the request that was sent:

```rust
let estimate = context_usage
    .estimate(EstimateRequest { request_id, projection }, cancelled.clone())
    .await?;
let reserved = estimate.estimated_input_tokens;
// ... the final request is built and validated ...
context_usage.confirm(ConfirmRequest { request_id }).await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| ContextFacts (#2025, Session) | `describe`, `contributions`, `estimate` | The additive counter, the fixed contribution and one contribution's size. |
| ToolRegistry (#2026, Session) | `definitions`, `toolSourcesView.get` | The current tool declarations when the adopted configuration names no tools. |
| BranchScope | `timeline`, `dispatcher`, `branchLifecycle` | The line's facts, their order and its phase. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `read` | The effective context's numbers at a cutoff | `{through?}` | `{through, contextWindow, outputTokens, thresholdTokens, estimatedInputTokens, baselineSource}` | The caller cancels; the read stops and nothing changes | `not_configured`, `not_ready`, `unsupported`, `cancelled`, `closed` |
| `estimate` | The budget baseline of a request and the projection it counts | `{requestId, projection}` | `{estimatedInputTokens, baselineSource}` | The caller cancels before the count completes; nothing is saved | `not_configured`, `unsupported`, `cancelled`, `closed` |
| `confirm` | Save the contributions for a request that was sent with the estimate's projection | `{requestId}` | `{}` | None; the owner completes the save | `unknown` (no estimate for the id), `closed` |

`read` answers at `through`, or at the branch's current sequence when absent. `estimate` is idempotent for one request id and projection: a repeated call answers the same numbers. `confirm` makes those numbers the request's contribution; without it the estimate is discarded at the next request or at close. A saved result is written only for a source that actually changed the numbers; a processed but no-effect source only advances the in-memory position.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `not_configured` | `read`, `estimate` | Adopt a model first; without a configuration there are no numbers |
| `not_ready` | `read` | The next real model request establishes the totals; answer that usage is unavailable |
| `unsupported` | `read`, `estimate` | This model has no additive counter; keep only complete-request counts |
| `unknown` | `confirm` | No estimate was reserved for the id, or it was discarded; save nothing |
| `cancelled` | `read`, `estimate` | Nothing changed; retry at the same cutoff |
| `closed` | all | The line is closing; the next generation serves a reloaded line |

A missing predecessor makes the numbers unavailable rather than recounting history: `not_ready` is the caller's answer until an actual request establishes a new total. A real model preparation passes its projection and resolved counter to `estimate`; the numbers are saved only after `confirm` names the request that retained that projection.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `read` | Read the in-memory totals at the cutoff; a cold read seeks the saved totals by epoch, branch ordinal and source sequence | The read stops | Pending reads end `closed`; the next generation reloads the saved totals | ContextUsage |
| `estimate` | Count the projection once, or reuse the saved numbers that cover it, and record the association with the request id | Nothing is saved; the association is dropped | The association is dropped; contributions are saved only for confirmed requests | ContextUsage |
| `confirm` | Write the contributions the estimate counted, then keep the association for calibration | — | A request confirmed before close keeps its saved contributions; an unconfirmed one is discarded | ContextUsage |
| provider measurement | A settled `model/usage` for the associated request updates the baseline | — | A measurement for an unconfirmed request is ignored | ContextUsage |

## Exceptions with reasons

- `estimate` counts a projection, not a request: the request is built after the reservation, and counting the final request again is `validate`'s separate budget check.
- The provider baseline keeps one `Weak` reference to the verified request inside the owner: it is optional evidence, and losing it loses the fast path, never a saved number.
- A saved result and its position are one record: a separate per-source tree or a copy of the request body would duplicate what the facts already hold.

## Verification

- `read` answers the current totals, `not_configured` without a model and `not_ready` before the first real request; an unsupported counter answers `unsupported`.
- `estimate` for one request id and projection answers the same numbers twice; a changed projection counts again; an unchanged no-effect source writes no new record.
- `confirm` saves one record per changed source; a second `confirm` writes nothing new; `confirm` without an estimate is `unknown`.
- A settled `model/usage` calibrates the next request for the confirmed association; an incompatible control or content invalidates the association without a recount.
- A reload reads the latest saved totals and the covered position; an uncovered source makes `read` answer `not_ready` rather than recounting.
- A call in flight when the line closes ends `closed`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds the line's facts and writes its own saved records. |
| §2 scopes | Branch: the numbers belong to one line's effective context. |
| §3 orthogonality | No other owner's state is read; the counter and the tool declarations arrive through their owners' operations. |
| §4 rule 1 | No command event: the owner writes its own saved results. |
| §4 rule 2 | No handover: the numbers stay in the owner; `estimate` answers a value. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning: the budget rule stays with ContextPreparation. |
| §4 rule 5 | A saved record commits as one batch with its position. |
| §4 rule 6 | No context path: the numbers add no message. |
| §4 rule 7 | Callers use the three operations; the owner folds the public facts. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Saved numbers are derivable state; the facts remain authoritative. |
| §7 values, ids and objects | The projection crosses as data; the reply is a value; no handle crosses. |
| §7 one interface | One name, `contextUsage`, on the `ContextUsage` type; `ContextPreparation::usage` is removed. |
| §7 cross-language | Every operation is data. |
| ADR 002 D1 | Branch: the numbers are per-line. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2026 (ToolRegistry): `definitions` and `toolSourcesView.get` must answer the current declarations and their identity as data; ContextUsage reads them when the adopted configuration names no tools.
- #2025 (ContextFacts): `contributions` and `estimate` must accept the model selection as data, as the target contract states.

## Binding appendix

The service is marked on the `ContextUsage` type; the wire name stays `contextUsage`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BaselineSource {
    Counted,
    ProviderMeasured,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub through: u64,
    pub context_window: f64,
    pub output_tokens: f64,
    pub threshold_tokens: f64,
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimateRequest {
    pub request_id: String,
    pub projection: crate::history::StreamedContext,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmRequest {
    pub request_id: String,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsageError {
    #[error("not configured")]
    NotConfigured,
    #[error("not ready")]
    NotReady,
    #[error("unsupported counter")]
    Unsupported,
    #[error("no estimate for this request")]
    Unknown,
    #[error("cancelled")]
    Cancelled,
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl ContextUsage {
    pub async fn read(&self, request: ReadUsage, cancelled: CancelledFn) -> Result<Usage, UsageError> { /* .. */ }
    pub async fn estimate(&self, request: EstimateRequest, cancelled: CancelledFn) -> Result<Estimate, UsageError> { /* .. */ }
    pub async fn confirm(&self, request: ConfirmRequest) -> Result<(), UsageError> { /* .. */ }
}
```
