# ToolBinding

Scope: branch. Issue: #2028.

ToolBinding owns the branch's record of each request's adopted tool generation: the registry adoption the request declared, the declarations it was adopted for, the pins that keep a later prefix's generation alive, and the claims of the request's published calls. This document is the owner's target contract; the current implementation is described in the [ToolExecution plugin page](../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding).

## Use and scope: responsibility split

ToolBinding owns:

- **The request record.** One adopted generation per request id: the registry's adoption id, the declarations it was adopted for, and the turn the request belongs to. A request id is bound once; a budget-recovery request takes over the record of the request it retries.
- **Pins.** The records whose prefix a later request may still declare, and their release. A pin is a value; the owner releases an adoption when no later prefix can name it.
- **The claims of a request's published calls.** Before the batch fact is appended, ToolBinding asks the registry to register each call's claim, passing the execution context the contributor's claim callback needs; it releases the request's claims when the request's batch result is committed, when the turn ends, when the caller asks, or when the generation closes.
- **Settlement.** The record and its claims are settled by the owner from the line's facts: `step/tool-result` for the request's batch, `turn/end` for the turn's requests, and the generation's close. `release` only settles a request sooner; no end path depends on a caller calling it.

ToolBinding does not execute tools, decide admission, hold a handler or a claim guard. The registry's `adopt`/`release`, `claim`/`release_claims` are the Session-side capability ToolBinding requests; execution, write-target resolution and ordering class belong to the registry's `run`, `resolve_write` and `ordering`. The adopted configuration fact (`context/configured`'s tool part) belongs to Prompt.

Loop calls ToolBinding for the request record (`bind`, `claim`, `transfer`, `retain_only`, `release`) and calls the registry only for `declarations`, which answers the current host and contributed declarations; that read is the fingerprint's too, and no adoption or claim goes through it.

**Why the registry and this owner stay separate.** The registry owns the Session's *resolvable generations*: the handlers and the version sets, shared by every line, with freshness from the registry's registrations; an adoption keeps its versions resolvable until released, and its id is bound to the adopting row. ToolBinding owns one line's *request record*: which adoption one request declared, and the claims of its calls, with freshness from that line's Timeline facts; the record ends at the request's terminal fact and the pin ends when a later prefix cannot name it. Merging would put a line's request state in the Session owner (forbidden by [ADR 002 D1](../../adr/002-plugin-scope.md)) or the contributors' handlers in the branch (the contributor's row owns them). The branch-facing interface is therefore ToolBinding's alone.

Callers keep as business:

- Which declarations a request's prefix carries, and where the declaration comes from: the prefix's adopted configuration, the registry's current set, or a host's selection over the registry's declarations.
- When a request is prepared, retried or abandoned; whether a turn is failed; how a model request's declarations are shaped.
- The tool-call batch: which calls a request publishes and when.

Targets: the sibling contracts this design depends on are unmerged. `toolRegistry` is a target, pending [#2103](https://github.com/arcships/dim/pull/2103); `prompt.configure` is a target, pending [#2102](https://github.com/arcships/dim/pull/2102); the mechanism obligation for cross-scope access is [#2127](https://github.com/arcships/dim/issues/2127).

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Bind a request to the adopted generation its prefix declared | No: the adoption must be released on every path that ends the request, and a caller that held the adoption id would have to release it itself on each error path | `bind` |
| Read the adoption and declarations to run a call | No: the record is the owner's state; the registry's `run` takes the adoption id | `adopted` |
| Read a request's declarations for a program that may only see them (Code Mode) | Yes: `adopted` answers the same record; the program takes the declaration part | Business composition |
| Register a published call's claim before the batch fact | No: the claim must exist before the fact and be released on every path that never dispatches; the contributor's refusal is the claim's, not the caller's | `claim` |
| Keep the same adopted generation across a budget-recovery retry | No: the record moves to the new request id; composing it would copy and release records | `transfer` |
| Release a record when the request is done | No: the terminal fact is the line's; the caller may settle sooner but cannot see every end path | `release` (optional) |
| Keep only the current request after a rebuild | No: a rebuild must drop every stale record in one step | `retain_only` |

An operation exists only where its row says No.

## Caller example

Loop prepares a request from the prefix's adopted configuration and publishes a batch:

```rust
// The prefix's adopted tool part, or none when the configuration carries none.
match configuration.tools {
    Some(tools) => binding.bind(BindRequest {
        request_id: request.id.clone(),
        turn_id: turn.id.clone(),
        through,
        declared: tools.clone(),
        sources: configuration.tool_sources.clone(),
    }).await?, // AlreadyBound, Invalid, Unavailable, Closed
    None => {
        // No tool part: declare the registry's current host and contributed tools.
        let current = registry.declarations()?;
        binding.bind(BindRequest {
            request_id: request.id.clone(),
            turn_id: turn.id.clone(),
            through,
            declared: current.tools,
            sources: current.sources,
        }).await?
    }
}
// Before the batch fact, so a published question can already be answered.
binding.claim(ClaimRequest {
    request_id: request.id.clone(),
    calls: request.calls.clone(),
    context: execution.clone(),
}).await?; // Callback, NoRecord, Closed
// No release call: the owner settles the record and its claims when `step/tool-result`
// for this request, or `turn/end`, is dispatched. `release` settles sooner if Loop
// knows the request cannot run again.
```

A host that selects tools over the registry's declarations composes the same steps; the declarations it reads are the ones it binds:

```rust
let available = registry.declarations()?;
let settings = host.prepare(turn_id, request_id, &history, &available.tools)?;
binding.bind(BindRequest {
    request_id: request.id.clone(),
    turn_id: turn.id.clone(),
    through,
    declared: settings.tools.clone().unwrap_or_default(),
    sources: available.sources,
}).await?;
```

A budget-recovery retry keeps the generation the first attempt declared; `transfer`, `release` and `retain_only` are synchronous:

```rust
binding.transfer(TransferRequest { from: old_request_id, to: new_request_id })?; // NoRecord
```

ToolExecutor reads the adoption once per call and hands it to the registry, which resolves the version's handler:

```rust
let adopted = binding.adopted(context.request_id.clone())?; // NoRecord
let call = registry.resolve_write(ResolveWrite { adoption: adopted.adoption.clone(), call }, cancelled).await?;
let outcome = registry.run(RunCall {
    adoption: adopted.adoption,
    call,
    caller: context.clone(),
}, cancelled).await?; // the tool's own failure is a ToolResult, not an error
```

Code Mode reads the same record and takes only the declarations:

```rust
let declared: Vec<ToolDefinition> = binding.adopted(execution.request_id.clone())?.tools
    .into_iter()
    .filter(|tool| tool.name != CODEMODE_TOOL)
    .collect();
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` (Session, target, pending [#2103](https://github.com/arcships/dim/pull/2103)) | Adopt a declared generation and retain its versions; register a request's claims with their execution context; release an adoption, a request's claims and a request's retention. |
| `B:Prompt` (branch, target, pending [#2102](https://github.com/arcships/dim/pull/2102)) | Adopt the tool part of the branch's model configuration when the registry's set changes. |
| `B:Timeline`, `B:Dispatcher` | Reconcile the prefix's last configuration at branch opening; consume `step/tool-result` and `turn/end` for settlement; write nothing. |
| `B:BranchLifecycle` | Gate the registry-change watcher on activation and closing. |

The registry's `adopt`, `release`, `claim` and `release_claims` are called only by ToolBinding; Loop calls them through this owner and calls the registry only for `declarations`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `bind` | Fix a request to one adopted generation | `{requestId, turnId, through, declared, sources}` | `()` | None; the owner completes the adoption and the record | `already_bound`, `invalid`, `unavailable`, `closed` |
| `adopted` | The record's adoption id and declarations | `{requestId}` | `Adopted` | None | `no_record`, `closed` |
| `claim` | Register the calls' claims before their batch fact, with the context their callbacks need | `{requestId, calls, context}` | `()` | None | `no_record`, `callback`, `closed` |
| `release` | Settle a request now; the owner settles it at the terminal facts anyway | `{requestId}` | `()` | None | `closed` |
| `retain_only` | Keep one request's record and release every other | `{requestId}` | `()` | None | `closed` |
| `transfer` | Move a record to the request that retries it | `{from, to}` | `()` | None | `no_record`, `closed` |

`declared` is the request's full declaration list; `sources` is the prefix's `tool_sources` as version ids; `through` is the prefix cutoff the pin is released against; `turnId` is the turn the request belongs to and the key `turn/end` settles. `adoption` is the registry's id from `adopt`; the registry retains the version set until `release`. Replies are positional, as the published faces are.

### Errors

| Kind | Operation | Retryable | What the caller does next |
| --- | --- | --- | --- |
| `already_bound` | `bind` | No | A caller bug: the request id was bound twice; fail the turn as a fault. |
| `invalid` | `bind` | No | The registry refused the declarations; the error names the declaration and the reason; fail the turn and report them. |
| `unavailable` | `bind` | Yes, after re-preparing | The prefix declares a source version the registry no longer retains; re-prepare from the current declarations, or fail the turn. |
| `no_record` | `adopted`, `claim`, `transfer` | No | The request has no record: a caller bug or a request released before its batch; fail the turn as a fault. A reload settlement is not this error. |
| `callback` | `claim` | Depends on the cause | The contributor's claim callback refused; the error carries the call id, and the callback's own code in `reason` and its message in `message`. The batch is not published; the turn fails unless the cause says the call may be retried in a later request. |
| `closed` | any | No, in this generation | The branch is closing or reloading; the turn ends; a replacement generation serves the next request. |

`bind` errors carry the declaration that failed (`name`) or the source that is gone (`key`, `version`); the owner reports no failure the record cannot have.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `bind` | Check the record (a second bind adopts nothing), ask the registry to adopt the declarations, insert the record, release the adoptions of pins no later prefix declares | None: the call is synchronous and bounded | Nothing to release; the record is settled at the request's terminal fact or the close | A record in flight is refused with `closed`; the registry releases an adoption it granted at the row's unload | ToolBinding and ToolRegistry |
| `adopted` | Read the record and answer its adoption and declarations | None | Nothing to release | Refused with `closed`; a reload rebuilds the record from the prefix's configuration | ToolBinding |
| `claim` | Ask the registry to register a guard per call with the request's execution context; a refusal answers `callback` and registers nothing | None | The close releases the request's claims | Every claim of the generation is released | ToolBinding and ToolRegistry |
| `release` | Drop the record and ask the registry to release its claims and adoption | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |
| `retain_only` | Release every other record, in one pass | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |
| `transfer` | Move the record to the new request id | None | Nothing to release | Refused with `closed` once the generation closes | ToolBinding |
| Settlement | On the dispatched `step/tool-result` of the request: drop the record and release its claims; on `turn/end` of its turn: the same for every record of the turn; on close: every record and claim | — | — | The owner performs the same release on close | ToolBinding and ToolRegistry |

A record is settled before the branch accepts closing work. A reload rebuilds the record from the prefix's last adopted configuration, exactly as a fresh branch would; a request that was in flight when the owner reloaded has no record, and its executor ends the batch as a reload settlement, not as a `no_record` error.

## Exceptions with reasons

- Pins are values, not handles: a pin is a record whose adoption a later prefix may still name, and the registry keeps those versions resolvable until the owner releases the adoption at the next bind or the close. ToolBinding holds no handler and no guard.
- The record's settlement reads the line's facts (`step/tool-result`, `turn/end`) instead of depending on a caller's release call: only the owner of the record can guarantee every end path, and the facts are public. `release` and `retain_only` exist for Loop's budget-recovery and rebuild steps, not as the only settlement.

## Supersedes

- `docs/design/service-faces.md` §3.5's `toolBinding` row.
- `docs/design/contracts/runtime-owner-operations.md`'s "Loop, ToolExecutor and Code Mode readers → `toolBinding`" row.
- `docs/design/contracts/session-boundary.md` §A.5's ToolBinding adoption sentences.
- The current implementation page (`docs/architecture/rust-agent/plugins/ToolExecution.md#toolbinding`) and `tool-contributions.md` §7.5–7.6 keep describing the implementation until this contract lands.

## Verification

Flow tests on the owner:

- A request binds to the prefix's declaration; a tool registered after the bind does not appear in `adopted` or in the executor's run; a later request adopts the new registry set and the earlier pin's adoption is released.
- A second `bind` for the same request is `already_bound` and adopts nothing; the first adoption stays.
- A request whose declaration names a version the registry no longer retains fails `unavailable` and binds nothing.
- A claim exists before the batch fact and carries the request's `turnId`/`requestId`/`branchId`; a contributor that refuses answers `callback` with its own code and message and the batch is not published; the request's batch result, a turn end, a close and an explicit `release` each release the claims the request did not dispatch.
- Budget recovery transfers the record, and the retried request runs the version its first attempt declared; `retain_only` drops the other records.
- After a reload, the branch rebinds its prefix's configuration before the first request; an in-flight batch ends as a reload settlement.
- Cross-language: the same operations run over the face and in process, with the same replies and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ToolBinding is one branch part and a Capability: it injects `dispatcher` and `timeline` and folds the branch's opening and its settlement facts. |
| §2 scopes | Branch. The record, pins and claims are the branch's; the registry's adoption and retained versions are Session state the branch requests by id. |
| §3 orthogonality | ToolBinding reads the registry's public adopt/release/claim operations and Prompt's configure operation; no other owner's state is read. |
| §4 rule 1 | Binding, settling and transferring are commands to this owner; the observable facts stay with Prompt (`context/configured`) and the tools' results. |
| §4 rule 2 | No handoff: the request record is the owner's state. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | No batch. |
| §4 rule 6 | Declarations reach the model only through the adopted configuration; the record reads the same values. |
| §4 rule 7 | Callers use the operations; no caller reads the record's internals. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact of its own; the adopted configuration stays Prompt's fact, and settlement reads public facts. |
| §7 resources and objects | The record, pins and adoption ids are values; no handler or guard crosses. |
| §7 no proxy | ToolBinding returns its own record; execution and claims are ToolRegistry's operations, requested by this owner. |
| §7 one surface | The branch-facing operations are `bind`, `adopted`, `claim`, `release`, `retain_only` and `transfer`; `handler`, `take_claim`, `definitions`, `prepare` and `bind_current` are removed. |
| §7 cross-language | Every parameter and reply is data; the face and the Rust call use the same operations. |
| §7 responsibility split | The caller keeps the declaration source, the turn's decisions and the batch; the owner keeps the record, its pins, its claims and their settlement. |
| ADR 002 D1 | Branch scope for the request record; the registry's generations are Session state. |
| ADR 002 D2 | No wiring part; the two owners are split by scope, not by forwarding, and the branch-facing interface is this owner's. |
| ADR 002 D3 | The registry is a Session capability the branch depends on; the registry depends on no branch. |
| ADR 002 D4 | No configuration of its own; the adopted configuration is Prompt's fact. |
| ADR 002 D5 | Language-neutral operations and data. |
| ADR 027 | Not applicable: ToolBinding holds no cross-scope access; its caller Loop is a branch part. |

## Requirements raised to mechanism owners

- **ToolRegistry ([#2103](https://github.com/arcships/dim/pull/2103), target).** The registry's claim must take the request's execution context (`{request, calls, context}`) so the contributor's claim callback receives the turn, request and branch, and it must answer a refusal as its `callback` error carrying the callback's own code and message; ToolBinding maps that to its `callback` error. Its `adopt`, `release`, `claim` and `release_claims` are called only by ToolBinding; `declarations` stays the one read Loop and the fingerprint make.
- **Prompt ([#2102](https://github.com/arcships/dim/pull/2102), target).** The owner needs Prompt's `configure({tools})` operation, because ToolBinding writes no configuration fact; a tool-only adoption without an adopted model is Prompt's `invalid_model`.
- **Cross-scope mechanism ([#2127](https://github.com/arcships/dim/issues/2127), target).** No obligation from this owner; it is listed because the series' Session-to-line access depends on it.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `toolBinding`. Every operation completes without waiting; the registry's operations take the cancellation argument. `ToolDefinition` and `ToolSourceVersion` are `dim_agent_types` values; `ToolExecutionContext` is the crate's existing execution context (`dim_agent_capabilities::host`), not a type of this owner.

```rust
use crate::host::ToolExecutionContext;
use dim_agent_runtime::PublicService;
use dim_agent_types::{ToolCall, ToolDefinition, ToolSourceVersion};

/// One request's adopted generation: the registry's adoption and the
/// declarations it was adopted for.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adopted {
    pub adoption: String,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindRequest {
    pub request_id: String,
    pub turn_id: String,
    pub through: u64,
    pub declared: Vec<ToolDefinition>,
    pub sources: Vec<ToolSourceVersion>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimRequest {
    pub request_id: String,
    pub calls: Vec<ToolCall>,
    pub context: ToolExecutionContext,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BindingError {
    #[error("request {request_id} is already bound")]
    AlreadyBound { request_id: String },
    #[error("the tool declaration {name} was refused: {reason}")]
    Invalid { name: String, reason: String },
    #[error("tool source {key}@{version} is unavailable")]
    Unavailable { key: String, version: String },
    #[error("request {request_id} has no tool record")]
    NoRecord { request_id: String },
    #[error("the claim of call {call_id} was refused ({reason}): {message}")]
    Callback { call_id: String, reason: String, message: String },
    #[error("the branch is closing")]
    Closed,
}

impl PublicService for ToolBinding {}

#[rutis::service(name = "toolBinding")]
impl ToolBinding {
    pub async fn bind(&self, request: BindRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn adopted(&self, request_id: String) -> Result<Adopted, BindingError> { /* ... */ }
    pub async fn claim(&self, request: ClaimRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn release(&self, request_id: String) { /* ... */ }
    pub fn retain_only(&self, request_id: String) { /* ... */ }
    pub fn transfer(&self, request: TransferRequest) -> Result<(), BindingError> { /* ... */ }
}
```
