# Summary strategy

Scope: session. Issue: #2025.

The owner replaces a range of a branch's history with a summary produced by model calls: it picks the cut the retention rule allows, summarizes the transcript in bounded chunks, and reports what it did. Today the same need is the `compactionStrategyHost` object, and `CompactionInput` carries the context model object so the owner can count. This document replaces the object with two operations and removes the object field.

## Use and scope: responsibility split

The owner answers:

- **The plan.** The strategy's name and the planned token size of its replacement, so the caller can reserve the space before the strategy runs.
- **The replacement.** One compaction from a request at a cutoff: the chosen cut, the replacement messages, and the model calls, usage and duration it took.
- **The outcome as data.** `completed`, `skipped` or `failed`, each with diagnostics; a failure inside the strategy is an outcome, not an error.

Callers keep:

- When a compaction runs: the automatic threshold, the manual command and the recovery rule are ContextPreparation's and Loop's business.
- The legal cuts and the retention limit: the policy that computes them is the caller's.
- Publishing the compaction fact and saving a snapshot: the caller commits what the strategy reported.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Loop or ContextPreparation replaces a range of history when the budget is exceeded or the operator asks | No: the summary prompt, the chunking, the retry budget and the diagnostics are one policy; composing them in a caller duplicates the summarizer | `compact` |
| The retention rule reserves the replacement's planned size before running | No: the plan comes from the same target rule the prompt states, and only the owner knows it | `describe` |

## Caller example

```rust
// ContextPreparation: the planned size, then the compaction.
let plan = strategy.describe(DescribeRequest { model: selection.clone() }).await?;
let reserved = plan.replacement_size;
let outcome = strategy
    .compact(CompactionRequest {
        history,
        checkpoint,
        request: request.clone(),
        retain_limit,
        cuts,
        trigger: CompactionTrigger::Threshold,
        estimated_input_tokens,
        baseline_source,
        baseline_prompt_tokens,
        delta_tokens,
        provider_budget,
        prior: None,
    }, cancel.clone())
    .await?;
match outcome.outcome {
    StrategyOutcome::Completed { retain_from, replacement, diagnostics } => {
        write_compaction(retain_from, replacement, diagnostics).await?
    }
    StrategyOutcome::Skipped { reason, .. } => record_skip(reason),
    StrategyOutcome::Failed { error, diagnostics } => record_failed(error, diagnostics),
}
```

## Own contract

### Dependencies

- `S:contextFacts` — the limits and the token count the plan and the chunk budget need.
- `P:modelHost` — the summarizer's model calls, under the existing failure and credential policy.
- `P:providerConnectionResolver` — the connection each summarizer request uses; the connection lives only inside the call.
- `S:summaryPromptSettingsHost` — optional: the operator's system prompt override, read once when the owner loads.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `describe` | The strategy's name and the planned size of its replacement | `{model}` | `{name, replacementSize}` | None; the owner completes the read | `not_adopted`, `closed` |
| `compact` | Replace the history before a cut with a summary | `{history, checkpoint?, request, retainLimit?, cuts, trigger, estimatedInputTokens, baselineSource, baselinePromptTokens?, deltaTokens?, providerBudget?, prior?}` | `{outcome}`: `completed`, `skipped` or `failed`, each with diagnostics | Stops the model call in flight; the compaction then answers nothing and writes nothing | `cancelled`, `closed`, `invalid` |

`model` is a model selection; the owner asks `contextFacts` for its limits and counts. `retainLimit` is the cut the retention rule allows; `cuts` lists every legal cut in order; `request` is the request whose context is compacted.

`completed` carries `{retainFrom, replacement, diagnostics}`; `skipped` carries `{reason, diagnostics}`; `failed` carries `{error, diagnostics}`. `diagnostics` holds the model call count, the summed usage, the strategy's own record and the duration.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `cancelled` | `compact` | The call was stopped before any model work or by the caller's cancellation; the history is unchanged |
| `closed` | both | The owner is unloading; the compaction did not run |
| `invalid` | `compact` | Fix the input: no legal cut, a cut outside the history, a request without an adopted model |
| `not_adopted` | `describe` | Adopt the model again; without it the plan cannot be computed |

A strategy failure after model work is the `failed` outcome with its diagnostics, not an error: the caller writes the compaction fact with those diagnostics.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `describe` | The owner reads the limits and the planned size from `contextFacts` | A dropped call ends the read | `closed` | The owner |
| `compact` | The owner picks the cut, summarizes in chunks under the input budget, and answers the outcome with diagnostics | The model call in flight is stopped; the call answers nothing and the history is unchanged | An in-flight compaction ends `cancelled`; the generation answers no further call | The model call by the owner; the compaction fact by the caller |

## Exceptions with reasons

- `describe` and `compact` take the model selection as data and ask `contextFacts` for the limits: a model object in the input made the strategy hold another owner's counter, and a number in the input would freeze the capacity the request actually has.
- `failed` is an outcome rather than an error: the compaction fact reports the same diagnostics as a completion, and the caller writes that fact either way.
- `prior` carries the earlier pass of a re-planned compaction: the caller re-plans when the first pass does not fit, and the second pass must continue the same request budget.

## Verification

- A threshold compaction over the retention limit completes, skips or fails with diagnostics; a compaction with no legal cut is `invalid` and sends no model call.
- `describe` equals the target rule plus the empty wrapper's count; a caller that reserved that size keeps it.
- A cancel during the model call answers nothing and leaves the history unchanged.
- A re-planned compaction sums both passes' model calls and usage.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part publishing two operations. It writes no Timeline event and folds no state; the compaction fact is the caller's. |
| §2 scopes | Session: the summarizer's policy and its model calls are shared by the Session's branches. |
| §3 orthogonality | Reads `contextFacts` and the model services; writes nothing outside its own calls. |
| §4 rule 1 | No command event: the caller writes the compaction start and result facts. |
| §4 rule 2 | No handover of an object; the outcome is data. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The summary prompt and the target rule are this owner's configuration, not core behavior. |
| §4 rule 5 | No batches. |
| §4 rule 6 | The replacement returns to the caller, which places it in the compaction fact; the owner adds no second path. |
| §4 rule 7 | Callers use the two operations; no caller reads the owner's prompt or chunk state. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | The compaction fact is the durable record; the strategy writes none itself. |
| §7 values, ids and objects | Requests and replies are data; the model object no longer crosses. |
| §7 one interface | One name, `summaryStrategy`, on the owner type. |
| §7 cross-language | Every request and reply is data; the cancellation is the caller's dropped call. |
| §7 one change | The implementation task removes `compactionStrategyHost` and migrates both callers in one change. |
| ADR 002 D1 | Session: the policy has no branch state and serves every branch. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Depends on process services at an enclosing scope and on `contextFacts` at its own. |
| ADR 002 D4 | The target rule and the prompt are factory configuration; the operator's override is read at load. |
| ADR 002 D5 | Data contract; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
use dim_agent_capabilities::host::StrategyDiagnostics;
use dim_agent_runtime::{ModelSelection, PublicService};
use dim_agent_types::{
    AgentEvent, BaselineSource, CompactionCut, CompactionTrigger, HistoryEntry, Message,
    ModelRequest, ProviderOutputBudgetFacts,
};
use std::sync::Arc;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DescribeRequest {
    pub model: ModelSelection,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub name: String,
    pub replacement_size: f64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionCheckpoint {
    pub retain_from: String,
    pub replacement: Arc<[Message]>,
    pub context_baseline: Arc<[dim_agent_types::ContextBaselineEntry]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_parts: Option<dim_agent_types::ReplacementParts>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionRequest {
    pub history: Vec<HistoryEntry>,
    pub checkpoint: Option<CompactionCheckpoint>,
    pub request: ModelRequest,
    pub retain_limit: Option<String>,
    pub cuts: Vec<CompactionCut>,
    pub trigger: CompactionTrigger,
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
    pub baseline_prompt_tokens: Option<f64>,
    pub delta_tokens: Option<f64>,
    pub provider_budget: Option<ProviderOutputBudgetFacts>,
    pub prior: Option<StrategyDiagnostics>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum StrategyOutcome {
    Completed {
        retain_from: String,
        replacement: Vec<Message>,
        diagnostics: StrategyDiagnostics,
    },
    Skipped {
        reason: String,
        diagnostics: StrategyDiagnostics,
    },
    Failed {
        error: String,
        diagnostics: StrategyDiagnostics,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionReply {
    pub outcome: StrategyOutcome,
}

#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    #[error("the compaction was cancelled")]
    Cancelled,
    #[error("the strategy owner is unloading")]
    Closed,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotAdopted(String),
}

impl PublicService for SummaryStrategy {}

#[rutis::service(name = "summaryStrategy")]
impl SummaryStrategy {
    pub async fn describe(&self, request: DescribeRequest) -> Result<Plan, StrategyError> { /* ... */ }
    pub async fn compact(
        &self,
        request: CompactionRequest,
        cancelled: dim_agent_capabilities::host::CancelledFn,
    ) -> Result<CompactionReply, StrategyError> { /* ... */ }
}
```

`cancelled` is the binding's cancellation argument, not a request field. `AgentEvent` is not part of the request: the caller writes the compaction facts.
