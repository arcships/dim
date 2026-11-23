# ToolBinding

Scope: branch. Issue: #2028.

ToolBinding owns the branch's record of each request's adopted tool generation: the registry adoption the request declared, the declarations it was adopted for, the pins that keep a later prefix's generation alive, and the claims of the request's published calls. This document is the owner's target contract; the current implementation is described in the [ToolExecution plugin page](../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding).

## Use and scope: responsibility split

ToolBinding owns:

- **The request record.** One adopted generation per request id: the registry's adoption id, the declarations it was adopted for, and the turn the request belongs to. A request id is bound once; a budget-recovery request takes over the record of the request it retries.
- **Pins.** The records whose prefix a later request may still declare, and their release. A pin is a value; the owner releases an adoption when no later prefix can name it.
- **The claims of a request's published calls.** Before the batch fact is appended, ToolBinding asks the registry to register each call's claim; it releases the request's claims when the request's batch result is committed, when the turn ends, or when the generation closes.
- **Settlement.** The record and its claims are settled by the owner from the line's facts: `step/tool-result` for the request's batch, `turn/end` for the turn's requests, and the generation's close. No caller releases on an end path.

ToolBinding does not execute tools, decide admission, hold a handler or a claim guard. The registry's `adopt`/`release`, `claim`/`release_claims` are the Session-side capability ToolBinding requests; execution, write-target resolution and ordering class belong to the registry's `run`, `resolve_write` and `ordering`. The adopted configuration fact (`context/configured`'s tool part) belongs to Prompt.

**Why the registry and this owner stay separate.** The registry owns the Session's *resolvable generations*: the handlers and the version sets, shared by every line, with freshness from the registry's registrations; an adoption keeps its versions resolvable until released, and its id is bound to the adopting row. ToolBinding owns one line's *request record*: which adoption one request declared, and the claims of its calls, with freshness from that line's Timeline facts; the record ends at the request's terminal fact and the pin ends when a later prefix cannot name it. Merging would put a line's request state in the Session owner (forbidden by [ADR 002 D1](../../adr/002-plugin-scope.md)) or the contributors' handlers in the branch (the contributor's row owns them). The branch-facing interface is therefore ToolBinding's alone: Loop drives `bind`, `claim` and `transfer`, never the registry.

Callers keep as business:

- Which declarations a request's prefix carries, and where the declaration comes from: the prefix's adopted configuration, the registry's current set, or a host's selection over the registry's declarations.
- When a request is prepared, retried or abandoned; whether a turn is failed; how a model request's declarations are shaped.
- The tool-call batch: which calls a request publishes and when.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Bind a request to the adopted generation its prefix declared | No: the adoption must be released on every path that ends the request, and a caller that held the adoption id would have to release it itself on each error path | `bind` |
| Read the adoption and declarations to run a call | No: the record is the owner's state; the registry's `run` takes the adoption id | `adopted` |
| Read a request's declarations for a program that may only see them (Code Mode) | Yes: `adopted` answers the same record; the program takes the declaration part | Business composition |
| Register a published call's claim before the batch fact | No: the claim must exist before the fact and be released on every path that never dispatches | `claim` |
| Keep the same adopted generation across a budget-recovery retry | No: the record moves to the new request id; composing it would copy and release records | `transfer` |
| Release a record when the request is done | No: the terminal fact is the line's, and the owner settles it without a caller call | owner settlement; no operation |

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
        let current = registry.declarations().await?;
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
}).await?;
// No release call: the owner settles the record and its claims when `step/tool-result`
// for this request, or `turn/end`, is dispatched.
```

A host that selects tools over the registry's declarations composes the same steps; the declarations it reads are the ones it binds:

```rust
let available = registry.declarations().await?;
let settings = host.prepare(turn_id, request_id, &history, &available.tools)?;
binding.bind(BindRequest {
    request_id: request.id.clone(),
    turn_id: turn.id.clone(),
    through,
    declared: settings.tools.clone().unwrap_or_default(),
    sources: available.sources,
}).await?;
```

A budget-recovery retry keeps the generation the first attempt declared:

```rust
binding.transfer(TransferRequest { from: old_request_id, to: new_request_id }).await?; // Invalid
```

ToolExecutor reads the adoption once per call and hands it to the registry, which resolves the version's handler:

```rust
let adopted = binding.adopted(&context.request_id).await?; // Invalid
let call = registry.resolve_write(ResolveWrite { adoption: adopted.adoption.clone(), call }, cancelled).await?;
let outcome = registry.run(RunCall {
    adoption: adopted.adoption,
    call,
    caller: context.clone(),
}, cancelled).await?; // the tool's own failure is a ToolResult, not an error
```

Code Mode reads the same record and takes only the declarations:

```rust
let declared: Vec<ToolDefinition> = binding.adopted(&execution.request_id).await?.tools
    .into_iter()
    .filter(|tool| tool.name != CODEMODE_TOOL)
    .collect();
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` (Session, [#2026](https://github.com/arcships/dim/issues/2026)) | Adopt a declared generation and retain its versions; register a request's claims; release an adoption, a request's claims and a request's retention. |
| `B:Prompt` (branch, [#2027](https://github.com/arcships/dim/issues/2027)) | Adopt the tool part of the branch's model configuration when the registry's set changes. |
| `B:Timeline`, `B:Dispatcher` | Reconcile the prefix's last configuration at branch opening; consume `step/tool-result` and `turn/end` for settlement; write nothing. |
| `B:BranchLifecycle` | Gate the registry-change watcher on activation and closing. |

The registry's `adopt`, `release`, `claim` and `release_claims` are called only by ToolBinding; Loop never calls the registry.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `bind` | Fix a request to one adopted generation | `{requestId, turnId, through, declared, sources}` | `{}` | None; the owner completes the adoption and the record | `already_bound`, `invalid` (the registry refused the declarations), `unavailable` (a source version is gone), `closed` |
| `adopted` | The record's adoption id and declarations | `{requestId}` | `{adoption, tools}` | None | `invalid` (no record), `closed` |
| `claim` | Register the calls' claims before their batch fact | `{requestId, calls, context}` | `{}` | None | `invalid` (no record), `closed` |
| `transfer` | Move a record to the request that retries it | `{from, to}` | `{}` | None | `invalid` (no record), `closed` |

`declared` is the request's full declaration list; `sources` is the prefix's `tool_sources` as version ids; `through` is the prefix cutoff the pin is released against; `turnId` is the turn the request belongs to and the key `turn/end` settles. `adoption` is the registry's id from `adopt`; the registry retains the version set until `release`.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `already_bound` | `bind` | A caller bug: the request id was bound twice. Fail the turn as a fault. |
| `invalid` | `bind` | The registry refused the declarations (a name has no handler, or its definition changed); fail the turn and report the declaration. |
| `unavailable` | `bind` | The prefix declares a source version the registry no longer retains; fail the turn, or re-prepare from the current declarations when the turn's policy allows a retry. |
| `invalid` | `adopted`, `claim`, `transfer` | The request has no record: a caller bug or a request released before its batch. Fail the turn as a fault. |
| `closed` | any | The branch is closing or unloading; the turn ends. |

`bind` errors carry the declaration that failed (`name`) or the source that is gone (`key`, `version`); the owner reports no failure the record cannot have.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `bind` | Ask the registry to adopt the declarations, insert the record, release the adoptions of pins no later prefix declares | None: the call is synchronous and bounded | Nothing to release; the record is settled at the request's terminal fact or the close | A record in flight is refused with `closed`; the registry releases an adoption it granted at the row's unload | ToolBinding and ToolRegistry |
| `adopted` | Read the record and answer its adoption and declarations | None | Nothing to release | Refused with `closed`; a reload rebuilds the record from the prefix's configuration | ToolBinding |
| `claim` | Ask the registry to register a guard per call; the record notes that the request has claims | None | The close releases the request's claims | Every claim of the generation is released | ToolBinding and ToolRegistry |
| `transfer` | Move the record to the new request id | None | Nothing to release | Refused with `closed` once the generation closes | ToolBinding |
| Settlement | On the dispatched `step/tool-result` of the request: drop the record and ask the registry to release the request's claims; on `turn/end` of its turn: the same for every record of the turn; on close: every record and claim | — | — | The owner performs the same release on close | ToolBinding and ToolRegistry |

A record is settled before the branch accepts closing work. A reload rebuilds the record from the prefix's last adopted configuration, exactly as a fresh branch would; a request that was in flight when the owner reloaded has no record, and its executor ends the batch as a reload settlement, not as an `invalid` error.

## Exceptions with reasons

- Pins are values, not handles: a pin is a record whose adoption a later prefix may still name, and the registry keeps those versions resolvable until the owner releases the adoption at the next bind or the close. ToolBinding holds no handler and no guard.
- The record's settlement reads the line's facts (`step/tool-result`, `turn/end`) instead of a caller's release call: only the owner of the record can guarantee every end path, and the facts are public.

## Supersedes

- `docs/design/service-faces.md` §3.5's `toolBinding` row.
- `docs/design/contracts/runtime-owner-operations.md`'s "Loop, ToolExecutor and Code Mode readers → `toolBinding`" row.
- `docs/design/contracts/session-boundary.md` §A.5's ToolBinding adoption sentences.
- The current implementation page (`docs/architecture/rust-agent/plugins/ToolExecution.md#toolbinding`) and `tool-contributions.md` §7.5–7.6 keep describing the implementation until this contract lands.

## Verification

Flow tests on the owner:

- A request binds to the prefix's declaration; a tool registered after the bind does not appear in `adopted` or in the executor's run; a later request adopts the new registry set and the earlier pin's adoption is released.
- A request whose declaration names a version the registry no longer retains fails `unavailable` and binds nothing.
- A claim exists before the batch fact; the request's batch result, a turn end and a close each release the claims the request did not dispatch; a dispatched call's guard is consumed by the registry's run; Loop calls no release on any path.
- Budget recovery transfers the record, and the retried request runs the version its first attempt declared.
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
| §7 one surface | The branch-facing operations are `bind`, `adopted`, `claim` and `transfer`; `handler`, `take_claim`, `definitions`, `release`, `retain_only`, `prepare` and `bind_current` are removed. |
| §7 cross-language | Every parameter and reply is data; the face and the Rust call use the same operations. |
| §7 responsibility split | The caller keeps the declaration source, the turn's decisions and the batch; the owner keeps the record, its pins, its claims and their settlement. |
| ADR 002 D1 | Branch scope for the request record; the registry's generations are Session state. |
| ADR 002 D2 | No wiring part; the two owners are split by scope, not by forwarding, and the branch-facing interface is this owner's. |
| ADR 002 D3 | The registry is a Session capability the branch depends on; the registry depends on no branch. |
| ADR 002 D4 | No configuration of its own; the adopted configuration is Prompt's fact. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **ToolRegistry ([#2026](https://github.com/arcships/dim/issues/2026)).** The registry's contract ([#2103](https://github.com/arcships/dim/pull/2103)) defines `adopt`, `release`, `claim`, `release_claims`, `run`, `resolve_write`, `ordering` and `declarations`. This owner calls `adopt`, `release`, `claim` and `release_claims`; `run`, `resolve_write` and `ordering` are ToolExecutor's. Loop calls this owner only. The split is the one stated under Use and scope: the registry keeps the Session's resolvable generations; this owner keeps the line's request record and claims.
- **Prompt ([#2027](https://github.com/arcships/dim/issues/2027)).** The owner needs Prompt's `configure({tools})` operation ([PromptAdoption](https://github.com/arcships/dim/pull/2102)), because ToolBinding writes no configuration fact; a tool-only adoption without an adopted model is Prompt's `invalid_model`.
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
#[serde(rename_all = "camelCase")]
pub struct BindRequest {
    pub request_id: String,
    pub turn_id: String,
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

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingError {
    #[error("request {request_id} is already bound")]
    AlreadyBound { request_id: String },
    #[error("the tool declaration was refused: {reason}")]
    Invalid { reason: String },
    #[error("tool source {key}@{version} is unavailable")]
    Unavailable { key: String, version: String },
    #[error("the branch is closing")]
    Closed,
}

impl PublicService for ToolBinding {}

#[rutis::service(name = "toolBinding")]
impl ToolBinding {
    pub async fn bind(&self, request: BindRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn adopted(&self, request_id: String) -> Result<Adopted, BindingError> { /* ... */ }
    pub async fn claim(&self, request: ClaimRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn transfer(&self, request: TransferRequest) -> Result<(), BindingError> { /* ... */ }
}
```
