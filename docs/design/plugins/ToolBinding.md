# ToolBinding

Scope: branch. Issue: #2028.

ToolBinding owns the branch's record of each request's adopted tool set: the declarations the request's prefix declared, the source version ids the owner resolved them against, and the pins that keep a later prefix's versions resolvable. This document is the owner's target contract; the current implementation is described in the [ToolExecution plugin page](../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding).

## Use and scope: responsibility split

ToolBinding owns:

- **The request record.** One adopted value per request id: the declarations, the fixed tool names and the source version ids. A request id is bound once; a budget-recovery request takes over the record of the request it retries.
- **Pins.** The adopted sets a later request prefix may still declare, and their release. A pin is a value; ToolBinding asks the registry to retain the versions and drops the retention when the pin is released.
- **The claims of a request's published calls.** Before the batch fact is appended, ToolBinding asks the owner of the tool handlers to register each call's claim under the call's identity, and it releases every claim of the request at the request record's release.
- **The adopted value the executor passes back to the registry.** ToolBinding is the only place a request's adopted value is read after preparation.

ToolBinding does not execute tools, decide admission, hold a handler or a claim guard, or write the adopted configuration. The adopted configuration fact (`context/configured`'s tool part) belongs to Prompt; execution, write-target resolution, ordering class and the claim guards belong to ToolRegistry.

Callers keep as business:

- Which declarations a request's prefix carries, and where the declaration comes from: the prefix's adopted configuration, the registry's current set, or a host's selection over the registry's declarations.
- When a request is prepared, retried or abandoned; whether a turn is failed; how a model request's declarations are shaped.
- The tool-call batch: which calls a request publishes and when.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Bind a request to the adopted tool set its prefix declared | No: the record must be released on every path that ends the request, and a caller that held the adopted value would have to release it itself on each error path | `bind` |
| Read the adopted value to execute a call | No: the adopted value is the record's state, and the registry retains the versions on the record's behalf | `adopted` |
| Read a request's declarations for a program that may only see them (Code Mode) | Yes: `adopted` answers the same value; the program takes the declaration part | Business composition |
| Register a published call's claim before the batch fact | No: the claim must exist before the fact and be released with the record on every non-dispatch path | `claim` |
| Release a request that will not execute again | No: only the caller knows the turn's decision; the record owner performs the release | `release` |
| Keep the same adopted set across a budget-recovery retry | No: the record moves to the new request id; composing it would copy and release records | `transfer` |
| Keep only the current request after a rebuild | No: a rebuild must drop every stale record in one step | `retain_only` |

An operation exists only where its row says No.

## Caller example

Loop prepares a request from the prefix's adopted configuration and publishes a batch:

```rust
// The prefix's adopted tool part, or none when the configuration carries none.
let adopted = match configuration.tools {
    Some(tools) => binding.bind(BindRequest {
        request_id: request.id.clone(),
        through,
        declared: tools.clone(),
        sources: configuration.tool_sources.clone(),
    }).await?, // AlreadyBound
    None => {
        // No tool part: declare the registry's current host and contributed tools.
        let current = registry.declarations().await?;
        binding.bind(BindRequest {
            request_id: request.id.clone(),
            through,
            declared: current.tools,
            sources: current.sources,
        }).await?
    }
};

// Before the batch fact, so a published question can already be answered.
binding.claim(ClaimRequest {
    request_id: request.id.clone(),
    calls: request.calls.clone(),
    context: execution.clone(),
}).await?;
```

A host that selects tools over the registry's declarations composes the same three steps; the declarations it reads are the ones it passes on:

```rust
let available = registry.declarations().await?;
let settings = host.prepare(turn_id, request_id, &history, &available.tools)?;
binding.bind(BindRequest {
    request_id: request.id.clone(),
    through,
    declared: settings.tools.clone().unwrap_or_default(),
    sources: available.sources,
}).await?;
```

ToolExecutor reads the adopted value once per call and hands it back to the registry, which resolves the handler for that version:

```rust
let adopted = binding.adopted(&context.request_id).await?; // NoRecord
let outcome = registry.execute(ExecuteRequest {
    adopted,
    call: call.clone(),
    context: context.clone(),
}, cancelled).await?; // the tool's own failure is a ToolResult, not an error
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Prompt` (branch, [#2027](https://github.com/arcships/dim/issues/2027)) | Adopt the tool part of the branch's model configuration when the registry's set changes. |
| `S:ToolRegistry` (Session, [#2026](https://github.com/arcships/dim/issues/2026)) | Retain an adopted version set for a request, register a call's claim, and release a request's retention and claims. |
| `B:Timeline`, `B:Dispatcher` | Reconcile the prefix's last configuration at branch opening and after a reload. |
| `B:BranchLifecycle` | Gate the registry-change watcher on activation and closing. |

The registry's adopt, retain, claim and release operations are not defined yet; they are stated in Requirements.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `bind` | Fix a request to one adopted tool set | `{requestId, through, declared, sources}` | `{}` | None; the owner completes the record | `already_bound`, `version_unavailable` (a declared source version is no longer retained), `no_handler` (a declared name has no implementation in the registry) |
| `adopted` | The request's adopted value | `{requestId}` | `{adopted}` | None | `no_record` |
| `claim` | Register the calls' claims before their batch fact | `{requestId, calls, context}` | `{}` | None | `no_record` |
| `release` | Drop a request's record, its retention and its claims | `{requestId}` | `{}` | None | None: an absent record is a no-op |
| `transfer` | Move a record to the request that retries it | `{from, to}` | `{}` | None | `no_record` |
| `retain_only` | Keep one request's record and drop every other | `{requestId}` | `{}` | None | None |

`declared` is the request's full declaration list; `sources` is the prefix's `tool_sources` as version ids. `adopted` is the registry's adopted value: the declarations, the fixed tool names and the version ids the registry retained for the request. `context` is the call's execution context (`turnId`, `requestId`, `branchId`), the identity the registry keys claims by.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `already_bound` | `bind` | A caller bug: the request id was bound twice. Fail the turn as a fault. |
| `version_unavailable` | `bind` | The prefix declares a source version that is no longer retained; fail the turn, or re-prepare the request from the current declarations when the turn's policy allows a retry. |
| `no_handler` | `bind` | The declaration names a tool no source provides; fail the turn and report the declaration. |
| `no_record` | `adopted`, `claim`, `transfer` | The request has no record: a caller bug or a request released before its batch. Fail the turn as a fault. |
| `closed` | any operation | The branch is closing or unloading; the turn ends. |

`bind` errors carry the declaration that failed (`name`, `key`, `version`); the owner never reports a failure the record cannot have.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `bind` | Ask the registry to retain the versions and resolve the declarations, then insert the record; release pins no later prefix declares | None: the call is synchronous and bounded | Nothing to release; the record stays until `release` or the owner closes | A record in flight is refused with `closed`; the registry retention of an unfinished adopt is released by the registry | ToolBinding and ToolRegistry |
| `claim` | Ask the registry to register a guard per call under the call's identity; the record notes that the request has claims | None | The record's `release` drops the claims at the owner's close | Every claim of the generation is released; the registry drops its guards | ToolRegistry |
| `release` | Drop the record, ask the registry to release the request's retention and claims, then release pins whose prefix can no longer declare them | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |
| `transfer` | Move the record to the new request id, asking the registry to move the retention | None | Nothing to release | Refused with `closed` once the generation closes | ToolBinding and ToolRegistry |
| `retain_only` | Release every other record, in one pass | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |

A record is released before the branch accepts closing work. A reload rebuilds the record from the prefix's last adopted configuration, exactly as a fresh branch would; a request that was in flight when the owner reloaded has no record, and its executor ends the batch as a reload settlement, not as a `no_record` error.

## Exceptions with reasons

- Pins are values, not handles: a pin is the set of version ids a later prefix may declare, and the registry keeps those versions resolvable while the record holds the ids. The joint retention rule is stated in Requirements; ToolBinding holds no handler and no guard.

## Verification

Flow tests on the owner:

- A request binds to the prefix's declaration; a tool registered after the bind does not appear in `adopted` or in the executor's run; a later request adopts the new registry set and the earlier pin is released.
- A request whose declaration names a version the registry no longer retains fails `version_unavailable` and binds nothing.
- A published call's claim exists before the batch fact; a batch that never dispatches (append refused, interrupt before dispatch, admission refused, a Loop error, a reload) releases it with the record; a dispatched call's guard is consumed by the registry's execution.
- Budget recovery transfers the record, and the retried request executes the version its first attempt declared.
- After a reload, the branch rebinds its prefix's configuration before the first request; an in-flight batch ends as a reload settlement.
- Cross-language: the same operations run over the face and in process, with the same replies and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ToolBinding is one branch part and a Capability: it injects `dispatcher` and `timeline` and folds the branch's opening. |
| §2 scopes | Branch. The record and pins are the branch's; the registry's retained versions are Session state the branch requests by request id. |
| §3 orthogonality | ToolBinding reads the registry's public adopt/declarations operations and Prompt's adopt operation; no other owner's state is read. |
| §4 rule 1 | The round's decisions (bind, release, transfer) are commands to this owner; the observable facts stay with Prompt (`context/configured`) and the tools' results. |
| §4 rule 2 | No handoff: the request record is the owner's state. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | No batch. |
| §4 rule 6 | Declarations reach the model only through the adopted configuration; the record reads the same values. |
| §4 rule 7 | Callers use the operations; no caller reads the record's internals. |
| §4 rule 8 | No infrastructure change; the registry retention is an owner operation, not a mechanism change. |
| §4 rule 9 | No Timeline fact of its own; the adopted configuration stays Prompt's fact. |
| §7 resources and objects | The record, pins and claims are ids and values; no handler or guard crosses. |
| §7 no proxy | ToolBinding returns its own record; execution is ToolRegistry's operation. |
| §7 one surface | The operations are on the owner type; `handler`, `take_claim`, `definitions` and the preparation entries are removed. |
| §7 cross-language | Every parameter and reply is data; the face and the Rust call use the same operations. |
| §7 responsibility split | The caller keeps the declaration source and the turn's decisions; the owner keeps the record, retention and claim lifecycle. |
| ADR 002 D1 | Branch scope: a request's tool set is one behavior line's state. |
| ADR 002 D2 | No wiring part; the service is on the owner. |
| ADR 002 D3 | The registry is a Session capability the branch depends on; the registry depends on no branch. |
| ADR 002 D4 | No configuration of its own; the adopted configuration is Prompt's fact. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **ToolRegistry ([#2026](https://github.com/arcships/dim/issues/2026)).** The owner needs, on the registry's contract: `adopt({requestId, declared, sources}) → {adopted}` that resolves the declarations, retains the version set for the request and answers the adopted value; `release_request({requestId})` that drops the request's retention and its claims; `claim({requestId, calls, context})` that registers a guard per call and keeps it until the call is dispatched or the request is released; `declarations() → {tools, sources}` that answers the current host and contributed declarations in one read; and `execute({adopted, call, context}, cancelled) → {result}` that resolves the handler for the adopted version, takes the call's claim, executes and settles it. These match the target shape in #2026's scan report; the registry's contract must state the claim's lifecycle and the retention rule ToolBinding relies on.
- **Prompt ([#2027](https://github.com/arcships/dim/issues/2027)).** The owner needs Prompt's branch operation that adopts the tool part of the configuration (`{tools, sources} → {adopted|unchanged|superseded}`), because ToolBinding writes no configuration fact.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `toolBinding`. `CancelledFn` is the crate's cancellation argument ([`host::CancelledFn`](../../../crates/dim-agent-capabilities/src/host.rs)); every operation here completes without waiting, so no operation takes it except the registry's execution.

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::{ToolCall, ToolDefinition, ToolSourceVersion};

/// The registry's adopted value for one request: its declarations, its fixed
/// tools and the source versions retained for the request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedTools {
    pub tools: Vec<ToolDefinition>,
    pub sources: Vec<ToolSourceVersion>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindRequest {
    pub request_id: String,
    pub through: u64,
    pub declared: Vec<ToolDefinition>,
    pub sources: Vec<ToolSourceVersion>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRequest {
    pub request_id: String,
    pub calls: Vec<ToolCall>,
    pub context: ToolExecutionContext,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("request {request_id} is already bound")]
    AlreadyBound { request_id: String },
    #[error("tool source {key}@{version} is unavailable")]
    VersionUnavailable { key: String, version: String },
    #[error("tool {name} has no handler")]
    NoHandler { name: String },
    #[error("request {request_id} has no tool record")]
    NoRecord { request_id: String },
    #[error("the branch is closing")]
    Closed,
}

impl PublicService for ToolBinding {}

#[rutis::service(name = "toolBinding")]
impl ToolBinding {
    pub async fn bind(&self, request: BindRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn adopted(&self, request_id: String) -> Result<AdoptedTools, BindingError> { /* ... */ }
    pub async fn claim(&self, request: ClaimRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn release(&self, request_id: String) { /* ... */ }
    pub fn transfer(&self, request: TransferRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn retain_only(&self, request_id: String) { /* ... */ }
}
```
