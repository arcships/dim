# ToolExecutor

Scope: branch. Issue: #2028.

ToolExecutor owns one committed tool batch on a line: its cancellation, its interrupt state, the child calls a program makes through it, and the batch's terminal `step/tool-result`. This document is the owner's target contract; the current implementation is described in the [ToolExecution plugin page](../../architecture/rust-agent/plugins/ToolExecution.md#toolexecutor).

## Use and scope: responsibility split

ToolExecutor owns:

- **The current batch.** One top-level batch at a time: its token, its interrupt state, the per-call admission and the ordered settlement of the calls.
- **Child calls.** A program's nested call is opened against the current batch, addressed by a gate id the owner issues; the owner writes the child's `step/tool-call`/`step/tool-result`, runs the same admission and execution path, and closes the gate and collects the child effects when the parent call settles. A program that ends early may close its gate and read the confirmed effects to word its result; the owner closes it again at the parent's settlement without effect.
- **The facts.** `step/tool-start` and `step/tool-completed` when progress is enabled, `step/tool-result` with the correlated results, and the child facts with their parent call id.
- **Reload and interrupt settlement.** An open batch settles as a reload settlement, or as not executed, when the generation reloads; an interrupt cancels the current batch.

ToolExecutor does not declare tools, decide admission, resolve a write target's facts, execute a handler or hold a handler. The adoption and declarations belong to ToolBinding; admission belongs to Approval; write-target resolution (`resolve_write`), ordering class (`ordering`), claims and execution (`run`) belong to ToolRegistry.

Targets: `toolRegistry` is a target, pending [#2103](https://github.com/arcships/dim/pull/2103); `toolBinding` and `approval` are this PR's; the mechanism obligation for cross-scope access is [#2127](https://github.com/arcships/dim/issues/2127).

Callers keep as business:

- When a batch's calls run, and what a failed call means for the turn.
- The program's own control flow: which child calls a program makes and how the program reacts to a child result.
- The model-facing text of a tool failure: the owner records the result it is given.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Run a committed batch | No: the batch's order, admission, cancellation and terminal fact are one state machine | Loop writes `step/tool-call`; the owner consumes it (no caller operation) |
| Open a child call against the current batch | No: a program's child must belong to the running parent call, and a caller-composed id could outlive or precede it | `open` |
| Run a child call and receive its result | No: the child must take the same admission and execution path, write its own facts and settle before the parent | `invoke` |
| Collect the child effects before the parent result | No: only the owner knows which gates belong to the parent call and when they settled | `close` returns the confirmed effects; the owner merges them again at parent settlement |
| Read the request's declarations for the program | Yes: ToolBinding's `adopted` answers the declarations | Business composition |

An operation exists only where its row says No.

## Caller example

Code Mode opens a gate for one outer call and invokes child tools through it:

```rust
let gate = executor.open(&ctx, OpenRequest {
    parent_call_id: outer_call_id.clone(),
    context: execution.clone(),
}, cancelled.clone()).await?; // Closed, Interrupted, NoBatch, Cancelled
// The program's tools.call:
let result = executor.invoke(&ctx, InvokeRequest {
    gate_id: gate.gate_id.clone(),
    call,
}, cancelled.clone()).await?; // GateClosed, Closed, Cancelled; a tool failure is result.is_error
// The program ended: take the confirmed effects to word the result.
let effects = executor.close(&ctx, CloseRequest { gate_id: gate.gate_id }).await?; // GateClosed, Closed
```

A program's declarations come from the record owner, not the executor:

```rust
let adopted = binding.adopted(execution.request_id.clone())?; // NoRecord
let declared: Vec<ToolDefinition> = adopted.tools
    .into_iter()
    .filter(|tool| tool.name != CODEMODE_TOOL)
    .collect();
```

The owner closes the gate when the parent call settles; a program that returns early closes it early to read the effects, and the effects of every child call are merged into the parent call's result in invocation order either way.

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` (Session, target, pending [#2103](https://github.com/arcships/dim/pull/2103)) | `resolve_write` before admission, `ordering` for the batch's steps, and `run` for an admitted call. |
| `B:Approval` | Admit each call, including the user's answer and the policy recheck. |
| `B:Timeline`, `B:Dispatcher` | Commit progress and results; consume batches, interrupts and turn boundaries; report faults. |
| `B:TimelineIdentity` | The calling line's identity in the execution context and the span. |
| `B:BranchLifecycle` | Child tokens under the generation's lifetime. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `open` | Open a child-call gate for one outer call of the current batch | `{parentCallId, context}` | `OpenReply` | The caller's token before the gate id is issued; nothing was created | `closed`, `interrupted`, `no_batch`, `cancelled` |
| `invoke` | Run one child call through the same admission and execution path | `{gateId, call}` | `ToolResult` | The caller's token before the child settles ends the call with `cancelled`; the child is cancelled and its fact records it; the gate stays usable | `closed`, `gate_closed`, `cancelled` |
| `close` | Close the gate now and answer the confirmed child effects | `{gateId}` | `CloseReply` | None: the close settles | `closed`, `gate_closed` (the batch was interrupted or settled before the call) |

`caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, it is not a request field, and no caller sends or names a row. `gateId` is bound to the calling row and the parent call; a call from another row is `gate_closed`. `close` is idempotent for the owner's own settlement: the parent's settlement closes the gate whether or not the program did, and the effects reach the parent result once.

A child call's refusal, cancellation or failure is a `ToolResult` with `is_error: true`, not an operation error: the program decides what to do with it. `invoke`'s own `cancelled` is the caller's token, not the child's: the caller's call ends, the child's result carries the child's cancellation.

### Errors

| Kind | Operation | Retryable | What the caller does next |
| --- | --- | --- | --- |
| `closed` | `open`, `invoke`, `close` | No, in this generation | The executor is closing or reloading; stop the program; its batch is settled; a replacement generation serves the next request. |
| `interrupted` | `open` | No | The turn was interrupted; stop the program. |
| `no_batch` | `open` | No | The parent call is not part of the current batch; a caller bug: stop the program and report it. |
| `gate_closed` | `invoke`, `close` | No | The gate's parent call settled or the batch was interrupted; stop the program; the parent result carries the effects already confirmed. |
| `cancelled` | `open`, `invoke` | No | The caller's token ended the call; nothing was created for `open`; for `invoke`, the child's result records the child's own cancellation and the gate stays usable. |

Replies are positional: `open` answers the gate id, `invoke` the child's result, `close` the effects.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `open` | Check the current batch and the parent call, issue a gate id under the calling row, subscribe the gate to the parent token | Before the id is issued: nothing exists, `cancelled` | The row's gates are closed with their child tasks | New calls are refused; every gate of the generation is closed | ToolExecutor |
| `invoke` | Write the child `step/tool-call` with the parent call id, resolve the write target, admit the call, run it through the registry, write the child `step/tool-result`, record its effects | The call ends `cancelled`; the child is cancelled and its fact records the child's cancellation; the gate stays usable | The row's gates close; in-flight children end cancelled and are recorded | In-flight children end as reload settlements; the gate is closed | ToolExecutor for the facts and the gate; ToolRegistry for the handler and its claim; the calling row for the program future |
| `close` | Mark the gate closed, refuse new children, wait for its running children within the owner's grace, answer the confirmed effects | The close waits; a child past the grace is outside the contract | The row's gates close with their children | A close in flight finishes; the parent settlement closes the gate again | ToolExecutor |
| Parent settlement | After the parent call's result is known, close every gate of the parent, wait for its children within the owner's grace, and merge their effects into the result in invocation order | — | — | — | ToolExecutor |

The owner never hands out a gate: `gateId` names the gate only for `invoke` and `close`, and the owner closes it. A batch's calls settle in step order, each call's admission runs when the batch starts, and an interrupt cancels the current token: those rules stay as they are, and the caller sees only the results.

## Exceptions with reasons

- A child call's failure is a result, not an operation error: a program may catch it, and the model sees the same `ToolResult` text a top-level call would produce. The caller's own cancellation is the exception, and it ends the `invoke` call.

## Supersedes

- `docs/design/service-faces.md` §3.5's `toolExecutor` row.
- `docs/design/contracts/runtime-owner-operations.md`'s "Nested tool execution → `toolExecutor`" row.
- The current implementation page (`docs/architecture/rust-agent/plugins/ToolExecution.md#toolexecutor`) keeps describing the implementation until this contract lands.

## Verification

Flow tests on the owner:

- A child call writes its own `step/tool-call`/`step/tool-result` with the parent call id; its effects appear in the parent result in invocation order; the outer result is written after its children settle.
- A program that returns early still has its child effects merged; a `close` before the program ends answers the confirmed effects, and the same effects appear once in the parent result.
- An interrupt cancels a running child and refuses a new gate; a reload settles an open child and open parent, and a reload-settled result fails the turn.
- A dropped `invoke` ends with `cancelled` and leaves the child's fact recording the child's cancellation; the gate still accepts the program's next call; a child's own failure is `Ok(ToolResult { is_error: true })`.
- `open` refuses a parent call that is not in the current batch and a second batch while one runs; `gate_closed` answers a call after the parent settled.
- The same operations over the face and in process answer the same results and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ToolExecutor is one branch part and a Capability: it injects `dispatcher` and `timeline` and consumes `step/tool-call`. |
| §2 scopes | Branch: a committed batch is one line's work. |
| §3 orthogonality | The executor reads ToolRegistry's and Approval's operations; no other owner's state. |
| §4 rule 1 | The batch is a command; the executor consumes it and writes results. |
| §4 rule 2 | The batch is handed to the executor by `step/tool-call`; nothing is taken back. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | One batch is one atomic result; the result is last. |
| §4 rule 6 | Results reach the model only through `step/tool-result`; child facts are excluded from context. |
| §4 rule 7 | Callers use the operations; handlers and claims stay with the registry. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Progress and results are facts later decisions use; the batch's completion is consumed by Loop and the client. |
| §7 resources and objects | The gate is the owner's; only its id crosses, and the owner closes it. |
| §7 no proxy | No forwarding of declarations; Code Mode reads ToolBinding. |
| §7 one surface | `open`, `invoke` and `close` are the operations; `definitions` and the gate object are removed. |
| §7 cross-language | Both operations take and answer data; a program in another language uses the same ids. |
| §7 responsibility split | The caller keeps the program's control flow and its reaction to results; the owner keeps order, admission, cancellation and facts. |
| ADR 002 D1 | Branch scope. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | Depends on Session capabilities (the registry) and branch peers (Approval); nothing depends on the executor's internals. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Language-neutral operations and data. |
| ADR 027 | Not applicable: the executor is reached by the branch that runs it, not across scopes. |

## Requirements raised to mechanism owners

- **ToolRegistry ([#2103](https://github.com/arcships/dim/pull/2103), target).** The registry's contract defines `resolve_write({adoption, call})`, `ordering({adoption, call})` and `run({adoption, call, caller}, cancelled)`. The executor uses those three; the tool's own failure comes back as a `ToolResult`, not as an error kind.
- **Cross-scope mechanism ([#2127](https://github.com/arcships/dim/issues/2127), target).** No obligation from this owner; it is listed because the series' Session-to-line access depends on it.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `toolExecutor`. `caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, it is not a request field, and no caller sends or names a row.

```rust
use crate::host::{CancelledFn, ToolExecutionContext};
use dim_agent_runtime::PublicService;
use dim_agent_types::{ToolCall, ToolEffect, ToolResult};
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenRequest {
    pub parent_call_id: String,
    pub context: ToolExecutionContext,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenReply {
    pub gate_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvokeRequest {
    pub gate_id: String,
    pub call: ToolCall,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseRequest {
    pub gate_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseReply {
    pub effects: Vec<ToolEffect>,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ExecutorError {
    #[error("the tool executor is closing")]
    Closed,
    #[error("the turn was interrupted: {reason}")]
    Interrupted { reason: String },
    #[error("no current tool batch accepts this child call")]
    NoBatch,
    #[error("the child gate is closed")]
    GateClosed,
    #[error("the call was cancelled")]
    Cancelled,
}

impl PublicService for ToolExecutor {}

#[rutis::service(name = "toolExecutor")]
impl ToolExecutor {
    pub async fn open(&self, caller: &Ctx, request: OpenRequest, cancelled: CancelledFn) -> Result<OpenReply, ExecutorError> { /* ... */ }
    pub async fn invoke(&self, caller: &Ctx, request: InvokeRequest, cancelled: CancelledFn) -> Result<ToolResult, ExecutorError> { /* ... */ }
    pub async fn close(&self, caller: &Ctx, request: CloseRequest) -> Result<CloseReply, ExecutorError> { /* ... */ }
}
```
