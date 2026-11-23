# ToolRegistry

Scope: session. Issue: #2026.

ToolRegistry owns a Session's tool offerings: the host tools, the tools and dynamic sources parts register, the generation each request runs, and the source status other parts observe. The current implementation is described in the [ToolRegistry plugin page](../../architecture/rust-agent/plugins/ToolRegistry.md). ToolBinding, ToolExecutor, Loop and the contributors are callers; their contracts are #2027, #2028 and their own.

## Use and scope: responsibility split

ToolRegistry owns:

- **The host tool table.** The tools the host offers this Session, read once from the host tools owner when the registry applies.
- **Registrations.** One tool or one dynamic source per registering row, with its declaration, its execute callback, its claim callback and its versions; a registration ends when its row unloads.
- **Generations.** A source declares complete sets under never-reused version ids; the registry adopts, rejects and withdraws whole sets, and keeps the versions a live adoption names.
- **Adoption.** The generation one request's prefix runs: the declarations it declares and the source versions it names. An adoption is an id bound to the adopting row.
- **The claim before the fact.** The pre-publish registration a call needs, keyed by `(request, call)`, routed to the contributor that claimed it, released when the request abandons the call or the row unloads.
- **The mount list.** `SessionConfig.tools` decides which host and registered tools are declared, fixed at creation.
- **The source status view.** Each source key's adopted version or rejection, published as a value and a subscription.

Callers keep:

- What a tool does: its declaration, schema, risk and execution, inside the contributor's part.
- Which tools a request declares: the adopted configuration carries them; the branch keeps the adoption id.
- Approval and permission: ToolExecutor and Permission decide; the registry decides nothing about whether a call may run.
- Result facts: ToolExecutor records them; the registry returns the result unchanged.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part publishes one tool for its row's lifetime | No: the registration's lifetime, the name and declaration checks and the host-name reservation are the owner's | `register` |
| A dynamic source declares complete sets under new versions and runs their tools | No: version uniqueness, whole-set validation, atomic replacement and routing a call to the source are the owner's | `register_source` |
| A request prefix fixes the generation it runs | No: resolving versions, checking every declared tool has a handler, and keeping the generation alive until release are the owner's | `adopt`, `release` |
| The branch executes one call of a request | No: the tool's owner runs it and the handler never leaves the registry; a caller holding the handler would carry the contributor's object | `run` |
| The branch resolves a write target before approval | No: only the owner knows which generation the request adopted and which handler answers | `resolve_write` |
| The branch orders a call within its batch | No: the class is a property of the adopted handler | `ordering` |
| A call needs a registration before `step/tool-call` and none if it never dispatches | No: the claim must exist before the fact and be released on every abandon path; composing it makes each caller hold the contributor's entry | `claim`, `release_claims` |
| A context fingerprint reads the current declarations and versions | No: both are the owner's state | `declarations` |
| A management view observes adopted and rejected versions | No: the status is the owner's | `sources`, `observe_sources` |
| Which tools a request declares | Yes: the adopted configuration is a value; the branch passes its source versions to `adopt` | Business |
| Approve, deny or cancel a call | Yes: ToolExecutor, Permission and Approval decide | Business |

An operation or parameter exists only where the row says No.

## Caller example

A part registers a tool and a claim callback; the registration lives with the part's row:

```rust
registry.register(
    ctx,
    ask_user_definition(),
    |call, context, cancelled| Box::pin(run_ask_user(call, context, cancelled)),
    |call, context| Box::pin(claim_question(call, context)), // inserts the waiter; no object comes back
).await?;
```

ToolBinding fixes a request's generation; ToolExecutor runs one of its calls:

```rust
let Adoption { adoption } = registry.adopt(ctx, settings.tools.clone(), settings.tool_sources.clone()).await?;
// ...
let result = registry.run(
    adoption.clone(),
    ToolCall { id, name, input },
    ToolExecutionContext { turn_id, request_id, branch_id },
    cancelled.clone(),
).await?;
// A refusal or a bad call is a ToolResult; only the kinds below reach this caller.
```

Before the call is published, ToolBinding claims it for Loop's request; ToolBinding releases the claims the request never dispatched at the request's terminal fact:

```rust
let claimed = registry.claim(ctx, request_id.clone(), calls.clone()).await?;
// A refused append, an interrupt before dispatch, a refused admission or a Loop error:
registry.release_claims(ctx, request_id.clone(), calls).await?;
```

A context fingerprint and a management view read values:

```rust
let Declarations { tools, sources } = registry.declarations()?;
let status = registry.sources()?; // adopted or rejected per source key
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:toolHost` | The host tools owner: declarations, execute, write-target resolution and ordering for the host's tools. |
| `S:sessionScope` | `config` for the mount list and the excluded names (fixed at creation), `identity` for the Session id in a foreign tool's neutral call and the attach span, and `activated`/`closing` for the refresh task. |

The registry provides `toolRegistry` on its owner type and nothing else; `toolSourcesView` leaves the catalog and its value is `sources`/`observe_sources` on the same face.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish one tool for the calling row's lifetime | `{declaration}`, an execute callback and a claim callback | `{}` | None; the owner completes it | `invalid`, `closed` |
| `register_source` | Publish a dynamic source: the sets it declares now and the callback that runs them | `{name}`, a sets callback and an execute callback | `{}` | None | `invalid`, `closed` |
| `adopt` | Fix the generation a request prefix runs | `{tools, sources}` | `{adoption}` | None; the owner completes it | `invalid`, `unavailable`, `closed` |
| `release` | Stop keeping an adoption's versions before the row unloads | `{adoption}` | `{released}` | None | `invalid`, `closed` |
| `run` | Run one call of an adopted generation | `{adoption, call, caller}` | `{result}` | The caller's token stops the call; the tool's own settlement stays with its contributor | `invalid`, `unavailable`, `cancelled`, `closed` |
| `resolve_write` | Resolve a write's target without effects, before admission | `{adoption, call}` | `{call}` | The caller's token stops the resolution | `invalid`, `unavailable`, `cancelled`, `closed` |
| `ordering` | The class deciding whether the call shares its step | `{adoption, call}` | `{class: "concurrent" \| "exclusive"}` | None | `invalid`, `unavailable`, `closed` |
| `claim` | Make the calls' pre-publish registrations exist before `step/tool-call` | `{request, calls}` | `{claimed: [callId]}` | None; the owner completes it | `invalid`, `callback`, `closed` |
| `release_claims` | Drop the claims a request never dispatched | `{request, calls}` | `{}` | None | `invalid`, `closed` |
| `declarations` | The current declarations and source versions | `{}` | `{tools, sources}` | None | None |
| `sources` | Each source key's adopted version or rejection | `{}` | `{sources}` | None | None |
| `observe_sources` | The status now and on each change until the row unloads | a listener callback | `{}` | The row's unload ends the subscription | `closed` |

The execute, claim, sets and listener callbacks are direct parameters, not request fields; they run outside the registry's locks and outside the Timeline's commit lock. A source's sets callback returns data (key, version, declarations); its execute callback receives `{key, tool, call, context}` and returns the result. `caller` is `{turnId, requestId, branchId}` data, not an object. A `ToolResult` a contributor returns is data and is returned unchanged; the tool's own failure is `isError`, not an owner error.

An adoption id and a claim's `(request, call)` entry are bound to the calling row: the row's unload releases them, so a caller that loses the id leaks nothing. `release` and `release_claims` only free them sooner.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a name outside `[A-Za-z0-9_-]{1,64}`, a host tool's name, a name registered already, a declaration without a risk or with an unknown field, a declared tool with no handler or a changed definition, an unknown adoption, request or claim |
| `unavailable` | The adopted source version is no longer this owner's; re-read the configuration and adopt again |
| `callback` | The contributor's claim callback refused; act on the callback's own `code` and `message` |
| `cancelled` | Nothing ran; the call stopped before the tool or the resolver started |
| `closed` | This Session is closing or this generation is unloading; a follow-up decision waits for the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register`, `register_source` | Validate, record under the row, offer the sets from the next read | — | The registration is withdrawn and its sets stop being offered | New registrations are refused; the task ends at `closing` | The registering row |
| `adopt` | Resolve every source version, check every declared tool, record the adoption under the row, answer the id | — | The row's adoptions are released with the row | New adoptions are refused; existing ones keep their versions until released | ToolRegistry |
| `run` | Resolve the adoption, pick the tool's callback, call it with the context and a child token, return the result | The child token fires; the contributor settles its own work | The call ends; the contributor's row unload settles it by the contributor's rule | New calls are refused; in-flight calls end `closed` or with the contributor's settlement | ToolRegistry for admission and the adoption; the contributor for its work |
| `resolve_write`, `ordering` | Resolve the adoption and ask the tool's callback | The resolution stops | The call ends | Refused `closed` | ToolRegistry |
| `claim` | Route each call to its tool's claim callback, record the contributor per `(request, call)`, answer the claimed ids | — | The row's claims are released with the row | New claims are refused; recorded claims are released | ToolRegistry for the record; the contributor for the pending entry |
| `release_claims` | Route the release to the contributor recorded for each call, drop the record | — | Nothing left to release | A release for an unloaded contributor is a no-op | ToolRegistry |
| `observe_sources` | Register the listener under the row and call it with the current status | The row's unload ends the subscription | The subscription ends; the callback's future is revoked | The listener stays until the generation unloads | ToolRegistry |

## Exceptions with reasons

- A tool's declaration, execute and claim callbacks are the contributor's code. The registry holds them under the contributor's row and never hands them out; a request names an adoption, not a handler.
- A source's sets callback returns declarations and version ids; the version's resources stay inside the source, and its execute callback routes by key and tool name. No handler object crosses the registration.
- A tool's own failure crosses as the `ToolResult` the contributor returns; the registry adds no error kind for it, and the caller reads `isError` and the content.
- A host tool's declaration is read once when the registry applies: the host tools owner's table is fixed for the Session, and a host tools reload reloads the registry with it.
- A foreign tool's execute callback holds the calling row's generation: a call after that row starts unloading is not made, and a call in flight ends with the contributor's reload message.

## Verification

Flow tests on the owner:

- A registration lives until its row unloads; a duplicate name, a host tool's name and an invalid declaration are `invalid`; a foreign declaration without a risk is `invalid`.
- `adopt` refuses a tool with no handler and a source version the owner no longer holds; a released adoption's versions stop being kept; a branch row's unload releases its adoptions.
- `run` uses the adopted generation after the source declares a newer one; a call whose contributor unloaded ends with the contributor's message; cancelling the call ends it `cancelled`.
- `claim` records each claiming contributor before the append; a claim callback's refusal answers `callback` with its own code; `release_claims` after a refused append leaves no entry; a dispatched call's entry lives until the answer or the cancel; a contributor's unload removes its entries.
- `declarations` and `sources` answer the current state; `observe_sources` delivers the current status and later changes; the row's unload ends the subscription.
- A call in flight when the Session starts closing ends `closed`; the refresh task starts at `activated` and ends at `closing`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline fact and folds none, so it is not a Capability. It injects the host tools owner and the Session scope. |
| §2 scopes | Session: the tool table and the adoptions belong to one Session; the branch's executor calls by adoption id and keeps no handler. |
| §3 orthogonality | No other owner's state is read or changed. A contributor that needs a new tool shape extends its own declaration; a caller that needs a new read asks here. |
| §4 rule 1 | No command event: a registration is the calling row's effect, not a Timeline fact. |
| §4 rule 2 | No handoff of a call: the registry routes the call and returns the result; ToolExecutor keeps the result's facts. |
| §4 rule 3 | No before callback: the execute, claim, sets and listener callbacks run outside the commit lock. |
| §4 rule 4 | Core and assembly carry no tool-specific meaning; a tool's declaration and result format are the contributor's. |
| §4 rule 5 | No batches: the registry adds no Timeline write. |
| §4 rule 6 | Declarations reach the model through the adopted configuration; results through ToolExecutor. The registry adds no context path. |
| §4 rule 7 | Callers use the operations; no caller reads the table, a version or a handler. |
| §4 rule 8 | No infrastructure change; the registration lifetime is the caller row's effect and the scope's wait is `sessionScope`. |
| §4 rule 9 | No Timeline fact; the source status is memory, read live. |
| §7 values, ids and callbacks | A registration, adoption, claim and status cross as data or ids; the execute, claim, sets and listener callbacks cross by reference; no handler, set object or provider object crosses. |
| §7 no proxy | The registry answers its own table and adoption; it does not answer another owner's declaration or result. |
| §7 one interface | One name, `toolRegistry`, on the owner type; `toolSourcesView` leaves the catalog; the Rust binding and the wire call the same methods. |
| §7 cross-language | Every operation is data, a callback or an id; no method exists only for Rust. |
| §7 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Session scope: the table, versions and adoptions live with one Session. |
| ADR 002 D2 | No wiring plugin: the operations are on the owner type; no `ForeignRegistry` wrapper remains. |
| ADR 002 D3 | A registration follows its part's lifetime; the registry depends on no contributor. |
| ADR 002 D4 | The mount list is structural; a business setting never selects tools. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **A tool call's claim needs one shared lifecycle.** The claim's owner is the contributor whose tool needs it; the branch's request record holds the `(request, call)` ids it claimed and owes a release on every path that does not dispatch; the tool table routes `claim`/`release_claims` by tool name and records the claiming contributor until release. Publish order is claim before `step/tool-call`; the transfer happens when ToolExecutor takes the call from the batch; the contributor's pending entry ends at the answer, the call's cancel or its own unload; the branch's release ends only the claims that never dispatched. This is one contract across the tool table, Questions and the branch request; it is stated here because the table is the directory that makes the routing possible.
2. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): `line_owner::<T>(line)` gives a Session part a branch owner's data face by id; registration points and lifecycle services are excluded. The tool parts that act on the calling line use it; this owner does not.

## Supersedes

- [tool-contributions §2 and §7.5](../../architecture/rust-agent/tool-contributions.md): the `ToolContribution`, `ToolHandler`, `ToolProvider` and `ToolSourceSet` shapes and the reader table are replaced by `register`, `register_source`, `adopt`, `release`, `run`, `resolve_write`, `ordering`, `claim`, `release_claims`, `declarations`, `sources` and `observe_sources`.
- [service faces §3.2](../service-faces.md): the `toolSourcesView` row leaves the catalog; its value is `sources`/`observe_sources` on `toolRegistry`.
- [session boundary A.5](../contracts/session-boundary.md): the `toolRegistry.register(declaration, execute, options?)` shape is replaced by `register(declaration, execute, claim)`; the persisted `ToolDefinition` shape is unchanged.

## Binding appendix

The service is marked on `ToolRegistry` under the wire name `toolRegistry`. `ForeignRegistry`, the `ToolHost` wrapper and `ToolSourcesView` leave the catalog; the host tools owner's operations stay under `toolHost`. `caller: &Ctx` is the rutis caller parameter for every registration or adoption bound to the calling row; `CancelledFn` is the binding's cancellation argument and is not a request field. A callback's failure arrives as `CallError`; `ToolCallbackError` keeps its own `code` and `message`.

```rust
use crate::host::CancelledFn;
use dim_agent_types::{ToolCall, ToolDefinition, ToolExecutionContext, ToolResult, ToolSourceVersion};
use rutis::host::{BoxFuture, CallError};
use rutis::Ctx;

pub type AdoptionId = String;

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ToolError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    Unavailable { message: String },
    #[error("{code}: {message}")]
    Callback { code: String, message: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the tool registry is unloading")]
    Closed,
}

/// A source's or a claim's own failure, carried unchanged.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct ToolCallbackError { pub code: String, pub message: String }

impl From<CallError> for ToolCallbackError {
    fn from(error: CallError) -> Self {
        match error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>()) {
            Some(remote) => Self { code: remote.code().unwrap_or("failed").into(), message: remote.message().into() },
            None => Self { code: "failed".into(), message: error.message().into() },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSet { pub key: String, pub version: String, pub tools: Vec<ToolDefinition> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCall { pub key: String, pub tool: String, pub call: ToolCall, pub context: ToolExecutionContext }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adoption { pub adoption: AdoptionId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Released { pub released: bool }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claimed { pub claimed: Vec<String> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Declarations { pub tools: Vec<ToolDefinition>, pub sources: Vec<ToolSourceVersion> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus { pub adopted: Option<String>, pub rejected: Option<ToolSourceRejection> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcesReply { pub sources: std::collections::BTreeMap<String, SourceStatus> }

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderClass { Concurrent, Exclusive }

#[rutis::service(name = "toolRegistry")]
impl ToolRegistry {
    pub async fn register(
        &self,
        caller: &Ctx,
        declaration: ToolDefinition,
        execute: impl Fn(ToolCall, ToolExecutionContext, CancelledFn) -> BoxFuture<'static, Result<ToolResult, ToolCallbackError>> + Send + Sync + 'static,
        claim: impl Fn(ToolCall, ToolExecutionContext) -> BoxFuture<'static, Result<bool, ToolCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), ToolError> { /* ... */ }

    pub async fn register_source(
        &self,
        caller: &Ctx,
        name: String,
        sets: impl Fn() -> BoxFuture<'static, Result<Vec<SourceSet>, ToolCallbackError>> + Send + Sync + 'static,
        execute: impl Fn(SourceCall, CancelledFn) -> BoxFuture<'static, Result<ToolResult, ToolCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), ToolError> { /* ... */ }

    pub async fn adopt(&self, caller: &Ctx, tools: Vec<ToolDefinition>, sources: Vec<ToolSourceVersion>) -> Result<Adoption, ToolError> { /* ... */ }
    pub async fn release(&self, caller: &Ctx, adoption: AdoptionId) -> Result<Released, ToolError> { /* ... */ }
    pub async fn run(&self, adoption: AdoptionId, call: ToolCall, context: ToolExecutionContext, cancelled: CancelledFn) -> Result<ToolResult, ToolError> { /* ... */ }
    pub async fn resolve_write(&self, adoption: AdoptionId, call: ToolCall, cancelled: CancelledFn) -> Result<ToolCall, ToolError> { /* ... */ }
    pub fn ordering(&self, adoption: AdoptionId, call: ToolCall) -> Result<OrderClass, ToolError> { /* ... */ }
    pub async fn claim(&self, caller: &Ctx, request: String, calls: Vec<ToolCall>) -> Result<Claimed, ToolError> { /* ... */ }
    pub async fn release_claims(&self, caller: &Ctx, request: String, calls: Vec<ToolCall>) -> Result<(), ToolError> { /* ... */ }
    pub fn declarations(&self) -> Result<Declarations, ToolError> { /* ... */ }
    pub fn sources(&self) -> Result<SourcesReply, ToolError> { /* ... */ }
    pub async fn observe_sources(&self, caller: &Ctx, listener: impl Fn(SourcesReply) -> BoxFuture<'static, ()> + Send + Sync + 'static) -> Result<(), ToolError> { /* ... */ }
}
```
