# ToolBinding

Scope: branch. Issue: #2028.

ToolBinding owns the branch's record of each request's adopted tool generation: the declarations the request's prefix declared, the registry adoption that keeps their versions resolvable, and the pins that keep a later prefix's generation alive. This document is the owner's target contract; the current implementation is described in the [ToolExecution plugin page](../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding).

## Use and scope: responsibility split

ToolBinding owns:

- **The request record.** One adopted generation per request id: the registry's adoption id and the declarations it was adopted for. A request id is bound once; a budget-recovery request takes over the record of the request it retries.
- **Pins.** The records whose prefix a later request may still declare, and their release. A pin is a value; ToolBinding asks the registry to release an adoption when no later prefix can name it.
- **The adopted value the executor runs and the program reads.** The adoption id and the declarations, read from the request record.

ToolBinding does not execute tools, decide admission, hold a handler, or register a call's claim. The registry's `adopt` resolves the declarations and retains their versions; the registry's `claim`/`release_claims` register and drop a request's pre-publish claims for Loop; execution, write-target resolution and ordering class belong to the registry's `run`, `resolve_write` and `ordering`. The adopted configuration fact (`context/configured`'s tool part) belongs to Prompt.

Callers keep as business:

- Which declarations a request's prefix carries, and where the declaration comes from: the prefix's adopted configuration, the registry's current set, or a host's selection over the registry's declarations.
- When a request is prepared, retried or abandoned; whether a turn is failed; how a model request's declarations are shaped.
- The tool-call batch and the claims of its calls: Loop calls `claim` before the batch fact and `release_claims` on the paths that never dispatch.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Bind a request to the adopted generation its prefix declared | No: the adoption must be released on every path that ends the request, and a caller that held the adoption id would have to release it itself on each error path | `bind` |
| Read the adoption and declarations to run a call | No: the record is the owner's state; the registry's `run` takes the adoption id | `adopted` |
| Read a request's declarations for a program that may only see them (Code Mode) | Yes: `adopted` answers the same record; the program takes the declaration part | Business composition |
| Release a request that will not execute again | No: only the caller knows the turn's decision; the record owner performs the release | `release` |
| Keep the same adopted generation across a budget-recovery retry | No: the record moves to the new request id; composing it would copy and release records | `transfer` |
| Keep only the current request after a rebuild | No: a rebuild must drop every stale record in one step | `retain_only` |

An operation exists only where its row says No.

## Caller example

Loop prepares a request from the prefix's adopted configuration and publishes a batch:

```rust
// The prefix's adopted tool part, or none when the configuration carries none.
match configuration.tools {
    Some(tools) => binding.bind(BindRequest {
        request_id: request.id.clone(),
        through,
        declared: tools.clone(),
        sources: configuration.tool_sources.clone(),
    }).await?, // AlreadyBound, Invalid, Unavailable, Closed
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
}
// Before the batch fact, so a published question can already be answered.
registry.claim(ClaimCalls { request: request.id.clone(), calls: request.calls.clone() }).await?;
```

A host that selects tools over the registry's declarations composes the same steps; the declarations it reads are the ones it binds:

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
| `S:ToolRegistry` (Session, [#2026](https://github.com/arcships/dim/issues/2026)) | Adopt a declared generation, retaining its versions, and release it when the record ends. |
| `B:Prompt` (branch, [#2027](https://github.com/arcships/dim/issues/2027)) | Adopt the tool part of the branch's model configuration when the registry's set changes. |
| `B:Timeline`, `B:Dispatcher` | Reconcile the prefix's last configuration at branch opening and after a reload. |
| `B:BranchLifecycle` | Gate the registry-change watcher on activation and closing. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `bind` | Fix a request to one adopted generation | `{requestId, through, declared, sources}` | `{}` | None; the owner completes the adoption and the record | `already_bound`, `invalid` (the registry refused the declarations), `unavailable` (a source version is gone), `closed` |
| `adopted` | The record's adoption id and declarations | `{requestId}` | `{adoption, tools}` | None | `invalid` (no record), `closed` |
| `release` | Drop a request's record and its adoption | `{requestId}` | `{}` | None | `closed` |
| `transfer` | Move a record to the request that retries it | `{from, to}` | `{}` | None | `invalid` (no record), `closed` |
| `retain_only` | Keep one request's record and drop every other | `{requestId}` | `{}` | None | `closed` |

`declared` is the request's full declaration list; `sources` is the prefix's `tool_sources` as version ids. `through` is the prefix cutoff the record's pin is released against. `adoption` is the registry's id from `adopt`; the registry retains the version set until `release`.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `already_bound` | `bind` | A caller bug: the request id was bound twice. Fail the turn as a fault. |
| `invalid` | `bind` | The registry refused the declarations (a name has no handler, or its definition changed); fail the turn and report the declaration. |
| `unavailable` | `bind` | The prefix declares a source version the registry no longer retains; fail the turn, or re-prepare from the current declarations when the turn's policy allows a retry. |
| `invalid` | `adopted`, `transfer` | The request has no record: a caller bug or a request released before its batch. Fail the turn as a fault. |
| `closed` | any | The branch is closing or unloading; the turn ends. |

`bind` errors carry the declaration that failed (`name`) or the source that is gone (`key`, `version`); the owner reports no failure the record cannot have.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `bind` | Ask the registry to adopt the declarations, insert the record, release the adoptions of pins no later prefix declares | None: the call is synchronous and bounded | Nothing to release; the record stays until `release` or the owner closes | A record in flight is refused with `closed`; an adoption the registry granted is released by the registry at the row's unload | ToolBinding and ToolRegistry |
| `adopted` | Read the record and answer its adoption and declarations | None | Nothing to release | Refused with `closed`; a reload rebuilds the record from the prefix's configuration | ToolBinding |
| `release` | Drop the record and ask the registry to release its adoption | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |
| `transfer` | Move the record to the new request id | None | Nothing to release | Refused with `closed` once the generation closes | ToolBinding |
| `retain_only` | Release every other record, in one pass | None | Nothing to release | The owner performs the same release on close | ToolBinding and ToolRegistry |

A record is released before the branch accepts closing work. A reload rebuilds the record from the prefix's last adopted configuration, exactly as a fresh branch would; a request that was in flight when the owner reloaded has no record, and its executor ends the batch as a reload settlement, not as an `invalid` error.

## Exceptions with reasons

- Pins are values, not handles: a pin is a record whose adoption a later prefix may still name, and the registry keeps those versions resolvable until the record releases the adoption. ToolBinding holds no handler and no guard.

## Verification

Flow tests on the owner:

- A request binds to the prefix's declaration; a tool registered after the bind does not appear in `adopted` or in the executor's run; a later request adopts the new registry set and the earlier pin's adoption is released.
- A request whose declaration names a version the registry no longer retains fails `unavailable` and binds nothing.
- A request's claims exist before the batch fact and are released on every path that never dispatches (append refused, interrupt before dispatch, admission refused, a Loop error); a dispatched call's guard is consumed by the registry's run. The claims are the registry's `claim`/`release_claims`, driven by Loop.
- Budget recovery transfers the record, and the retried request runs the version its first attempt declared.
- After a reload, the branch rebinds its prefix's configuration before the first request; an in-flight batch ends as a reload settlement.
- Cross-language: the same operations run over the face and in process, with the same replies and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ToolBinding is one branch part and a Capability: it injects `dispatcher` and `timeline` and folds the branch's opening. |
| §2 scopes | Branch. The record and pins are the branch's; the registry's adoption and retained versions are Session state the branch requests by id. |
| §3 orthogonality | ToolBinding reads the registry's public adopt/release operations and Prompt's configure operation; no other owner's state is read. |
| §4 rule 1 | The round's decisions (bind, release, transfer) are commands to this owner; the observable facts stay with Prompt (`context/configured`) and the tools' results. |
| §4 rule 2 | No handoff: the request record is the owner's state. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | No batch. |
| §4 rule 6 | Declarations reach the model only through the adopted configuration; the record reads the same values. |
| §4 rule 7 | Callers use the operations; no caller reads the record's internals. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact of its own; the adopted configuration stays Prompt's fact. |
| §7 resources and objects | The record, pins and adoption ids are values; no handler or guard crosses. |
| §7 no proxy | ToolBinding returns its own record; execution and claims are ToolRegistry's operations. |
| §7 one surface | The operations are on the owner type; `handler`, `take_claim`, `definitions`, `claim`, `prepare` and `bind_current` are removed. |
| §7 cross-language | Every parameter and reply is data; the face and the Rust call use the same operations. |
| §7 responsibility split | The caller keeps the declaration source, the turn's decisions and the claims; the owner keeps the record, its adoption and its pins. |
| ADR 002 D1 | Branch scope: a request's tool generation is one behavior line's state. |
| ADR 002 D2 | No wiring part; the service is on the owner. |
| ADR 002 D3 | The registry is a Session capability the branch depends on; the registry depends on no branch. |
| ADR 002 D4 | No configuration of its own; the adopted configuration is Prompt's fact. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **ToolRegistry ([#2026](https://github.com/arcships/dim/issues/2026)).** The registry's contract ([#2103](https://github.com/arcships/dim/pull/2103)) defines `adopt`, `release`, `run`, `resolve_write`, `ordering`, `claim`, `release_claims` and `declarations`. ToolBinding uses `adopt` and `release` only; Loop uses `claim` and `release_claims`; ToolExecutor uses `run`, `resolve_write` and `ordering`. The registry's row-bound unload release is what makes the record's adoption safe to hold as a value.
- **Prompt ([#2027](https://github.com/arcships/dim/issues/2027)).** The owner needs Prompt's `configure({tools})` operation ([PromptAdoption](https://github.com/arcships/dim/pull/2102)), because ToolBinding writes no configuration fact; a tool-only adoption without an adopted model is Prompt's `invalid_model`.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `toolBinding`. Every operation completes without waiting; the registry's operations take the cancellation argument.

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::{ToolDefinition, ToolSourceVersion};

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
    pub through: u64,
    pub declared: Vec<ToolDefinition>,
    pub sources: Vec<ToolSourceVersion>,
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
    pub fn release(&self, request_id: String) { /* ... */ }
    pub fn transfer(&self, request: TransferRequest) -> Result<(), BindingError> { /* ... */ }
    pub fn retain_only(&self, request_id: String) { /* ... */ }
}
```
