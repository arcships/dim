# Context facts

Scope: session. Issue: #2025.

The owner answers a model's context limits and how many tokens a request, a message or a request's fixed prefix take. Today the same need is the `contextModelHost` object: a `resolve(request)` call that returns an object whose methods the callers use one at a time. This document replaces the object with four reads. The owner has no state beyond its resolved counting rules and writes nothing.

## Use and scope: responsibility split

The owner answers:

- **The model's limits.** The context window, the output allowance every request reserves, the input threshold that triggers compaction, and the counting algorithm's identity when one exists.
- **A request's count.** The tokens of one complete request as the model's counter sees them.
- **A request's components.** The fixed contribution (system and tools), one contribution per message, and the estimate of a contribution.

Callers keep:

- What the budget decision is: ContextPreparation's threshold rule, the retention reserve and the fallback cuts.
- Which requests are counted and when a saved count may be reused: ContextUsage's statistics and its baseline association.
- How a summary's planned size is computed: the summary owner asks for the planned message's count.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| ContextPreparation decides whether a request is over the input budget | No: the limits, the reserve rule and the counter are the owner's knowledge; composing them in the caller duplicates the model's capacity facts | `describe`, `count` |
| Loop runs a compaction step and needs the window and the request's count | Yes: `describe` and `count` | Business composition |
| ContextUsage records a request's statistics additively | No: the counter's algorithm, its fixed contribution and its per-message contribution are one counting rule; splitting them across callers breaks the equality between `count` and the sum of the components | `contributions`, `estimate` |
| The summary owner plans the replacement size | Yes: `describe` for the algorithm and `count` for the planned message | Business composition |

## Caller example

```rust
// ContextPreparation: one request's budget.
let facts = facts.describe(DescribeRequest { model: selection.clone() }).await?;
let counted = facts.count(CountRequest { model: selection.clone(), request: request.clone() }).await?;
let over = counted.tokens > facts.threshold_tokens;
```

```rust
// ContextUsage: the contributions of one request, when the model has an additive counter.
let described = facts.describe(DescribeRequest { model: selection.clone() }).await?;
let Some(algorithm) = described.algorithm else {
    return Err(UsageError::NotCountable); // the caller keeps complete-request counts only
};
let parts = facts.contributions(ContributionsRequest {
    model: selection,
    fixed: Some(Fixed { system, tools }),
    messages,
}).await?;
let total: f64 = parts.fixed.map(|fixed| fixed.estimated).unwrap_or_default()
    + parts.messages.iter().map(|message| message.estimated).sum::<f64>();
```

## Own contract

### Dependencies

None. The capability snapshot, the config and the messages arrive in the request data; the model catalog is read by the profile that installs the owner, not while answering.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `describe` | The model's limits and counter identity | `{model}` | `{contextWindow, outputTokens, thresholdTokens, algorithm?}` | None; the owner completes the read | `not_adopted`, `closed` |
| `count` | One complete request's tokens | `{model, request}` | `{tokens}` | None | `not_adopted`, `closed` |
| `contributions` | The fixed and per-message contributions of a request | `{model, fixed?, messages}` | `{algorithm, fixed?, messages}` | None | `not_adopted`, `unsupported`, `closed` |
| `estimate` | The tokens of one contribution | `{model, algorithm, contribution}` | `{tokens}` | None | `not_adopted`, `unsupported`, `algorithm_mismatch`, `closed` |

`model` is the Session's model selection: provider, model, adopted capability snapshot, config and metadata. `contributions` answers contributions in request order and, when `fixed` is present, the fixed contribution of the given system and tools.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `not_adopted` | all | Adopt the model again and retry: the snapshot is missing, belongs to another provider or model, or the output allowance is absent |
| `unsupported` | `contributions`, `estimate` | This model has no additive counter; keep only complete-request counts |
| `algorithm_mismatch` | `estimate` | Recompute with the algorithm `describe` and `contributions` reported; a saved contribution of another algorithm cannot be estimated |
| `closed` | all | This generation is unloading; call the replacement generation |

`describe` and `count` do not fail for a model without an additive counter: `algorithm` is absent and `count` still answers.

### Flows

The four operations hold no resource and wait for nothing; their rows cover only owner close.

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `describe`, `count`, `contributions`, `estimate` | The owner resolves the model from the request data, counts and answers | A dropped call answers nothing and leaves nothing | An in-flight call ends `closed`; the generation answers no further call | The calling row drops the call; the owner closes |

## Exceptions with reasons

- `algorithm` is absent for a model whose count cannot be composed from contributions: reporting an algorithm it does not have would make `contributions` and `estimate` look available.
- `estimate` takes the algorithm as data: a caller that saved contributions in a Timeline fact reads them back after a restart, and the saved algorithm is the only proof that the numbers share one rule.
- `not_adopted` covers both a missing snapshot and a snapshot of another model: the caller's next step is the same, adopting the model again.

## Verification

- `describe` answers the window, allowance and threshold of an adopted model; a missing or foreign snapshot is `not_adopted`.
- `count` equals the fixed contribution plus the message contributions summed through `estimate`, for a model with an additive counter.
- `contributions` is `unsupported` for a model without a counter; `estimate` with another algorithm is `algorithm_mismatch`.
- A call in flight when the generation closes ends `closed`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part publishing four reads; it writes no Timeline event and folds no state. |
| §2 scopes | Session: the limits are properties of the model the Session runs, shared by its branches. |
| §3 orthogonality | No other owner's state is read; the model selection is data. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handover of an object: the reply is data. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning: the budget rule stays with ContextPreparation. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path. |
| §4 rule 7 | Callers use the reads; no caller holds a counter object. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact: counts are statistics, persisted by ContextUsage in its own record. |
| §7 values, ids and objects | Every reply is a value; the old `Arc<dyn ContextModel>` no longer crosses. |
| §7 one interface | One name, `contextFacts`, on the owner type. |
| §7 cross-language | Requests and replies are data. |
| ADR 002 D1 | Session: the model facts are shared by the Session's branches. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The profile that installs the owner reads its catalog; the owner injects nothing. |
| ADR 002 D5 | Data contract; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
use dim_agent_runtime::{ModelSelection, PublicService};
use dim_agent_types::{Message, ModelRequest, SystemPrompt, ToolDefinition};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DescribeRequest {
    pub model: ModelSelection,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub context_window: f64,
    pub output_tokens: f64,
    pub threshold_tokens: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountRequest {
    pub model: ModelSelection,
    pub request: ModelRequest,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountReply {
    pub tokens: f64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fixed {
    pub system: Option<SystemPrompt>,
    pub tools: Option<Vec<ToolDefinition>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributionsRequest {
    pub model: ModelSelection,
    pub fixed: Option<Fixed>,
    pub messages: Vec<Message>,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contribution {
    pub serialized_bytes: u64,
    pub images: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContributionsReply {
    pub algorithm: String,
    pub fixed: Option<f64>,
    pub messages: Vec<f64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimateRequest {
    pub model: ModelSelection,
    pub algorithm: String,
    pub contribution: Contribution,
}

#[derive(Debug, thiserror::Error)]
pub enum ContextFactsError {
    #[error("{0}")]
    NotAdopted(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    AlgorithmMismatch(String),
    #[error("{0}")]
    Closed(String),
}

impl PublicService for ContextFacts {}

#[rutis::service(name = "contextFacts")]
impl ContextFacts {
    pub async fn describe(&self, request: DescribeRequest) -> Result<Limits, ContextFactsError> { /* ... */ }
    pub async fn count(&self, request: CountRequest) -> Result<CountReply, ContextFactsError> { /* ... */ }
    pub async fn contributions(
        &self,
        request: ContributionsRequest,
    ) -> Result<ContributionsReply, ContextFactsError> { /* ... */ }
    pub async fn estimate(&self, request: EstimateRequest) -> Result<CountReply, ContextFactsError> { /* ... */ }
}
```

`ContributionsReply.fixed` and `.messages` are token counts, aligned with the request's messages; `Contribution` stays the serialized-bytes and image facts the counter works from.
