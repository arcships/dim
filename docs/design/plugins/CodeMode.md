# Code Mode

Scope: process, Session entry and branch run. Issue: #2033 (this contract); #1276 (delivery). The tool's contract, the child call batches, the approval rules and the client compatibility are unchanged and stay in [child tool batches](../appserver/child-tool-batches.md) and in this document's contract sections below. The current shape is the one #2033's scan report describes.

Code Mode lets the model submit one bounded QuickJS program that invokes the request's adopted tools and returns one ordinary tool result. Three owners hold its state: the process owns the program slots, the branch owns the one-shot run, and the Session entry declares the tool.

## Use and scope: responsibility split

Code Mode owns:

- **The structural bounds.** 256 KiB `max_code_bytes`, 600 s `max_runtime_ms`, 4096 `max_calls`, 16 `max_parallel_calls`, 1024 MiB `max_memory_bytes`, 2 `max_programs`, 16 MiB `max_output_bytes`, 4 MiB `max_single_result_bytes`. They are configuration, not tool parameters and not prompt text.
- **The process program slots.** At most `max_programs` programs run in one process at a time; the slot is held from the program's start to its settlement and released by the quota owner, including when the caller unloads.
- **The one-shot run.** One fresh QuickJS context per call with only `tools.list`, `tools.describe` and `tools.call`; the memory limit, the wall-clock interrupt and the program's cancellation; the outer result's text, effects and failure warning.
- **The tool declaration.** The `codemode` tool's name, description, schema and `session` risk. The v4 gate is the Session's mount exclusion: the connection adds `codemode` to the excluded tools for a v1–v3 client, and the entry registers the contribution regardless.

Callers keep as business:

- Which tools a request may call: ToolBinding pins the request's declarations, and the program sees only that snapshot without `codemode`.
- Whether a child call may run: Permission and Approval decide per child, with the tool's own risk.
- How child batches are shown: the client boundary and the TUI grouping.
- The prompt: the description states the contract the model acts on and nothing about host bounds.

| Requirement (caller) | Can callers compose it from the owner's operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model runs one program on the call's line | No: the QuickJS boundary, the memory limit and the interrupt must sit inside the run, on the line whose tools the program calls | `codeModeLine.run` at the call's line |
| At most `max_programs` programs per process | No: the cap is process state and must be settled when the run ends or the caller unloads | `codeMode.with_slot {work}` |
| Each child call is admitted on its own | No: admission, the child batch and the terminal result are ToolExecutor's | ToolExecutor's nested operation ([#2028](https://github.com/arcships/dim/issues/2028)); the program only names a tool and its arguments |
| A failed program returns a result the model reads | No: the failure text, the effects collected before it and the warning are the run's | the run's reply |
| Only v4 Sessions are offered the tool | No: the exclusion is the Session's creation record, set by the connection's protocol version | the mount exclusion `SessionConfig.excluded_tools`, as `exclude_codemode_below` sets it; the entry adds no version read |

An operation or parameter exists only where its row says No.

## Caller example

The entry's handler runs on the call's line and asks that line's run owner. No process table, no executor object, no slot handle.

```rust
// CodeModeSession's handler: the call's line is a value on the execution context.
let line = execution.line.clone();
let run = line_owner::<CodeModeLine>(&line)?;
let result = run.run(RunProgram {
    outer_call_id: call.id,
    code: input.code,
    context: execution,
}, cancelled).await?;
Ok(result)                                   // one ToolResult for the outer call
```

The line's run owner holds the slot around the program; the slot never leaves the quota owner.

```rust
// CodeModeLine: one program inside one process slot. The gate is the executor's;
// the run closes it early to word the result, and the owner settles it again.
let slot: Result<ToolResult, ServiceError> = quota
    .with_slot(caller, Arc::new(move |cancelled| Box::pin(async move {
        let gate = executor.open(caller, OpenRequest { parent_call_id: outer_call_id, context }, &cancelled)?;
        let outcome = quick_js(code, gate.gate_id.clone(), &cancelled).await;
        let effects = executor.close(caller, CloseRequest { gate_id: gate.gate_id })?;
        Ok(program_result(outcome, effects))  // the same effects appear once in the parent result
    })), cancelled)
    .await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| The id routing mechanism ([#2127](https://github.com/arcships/dim/issues/2127)) | `line_owner::<T>(line)` | The entry reaches `CodeModeLine`; the run reaches the line's ToolExecutor; #2024's contract states the line-id operations it serves ([target, pending #2092](https://github.com/arcships/dim/pull/2092)) |
| ToolExecutor ([#2028](https://github.com/arcships/dim/issues/2028), target, pending [#2100](https://github.com/arcships/dim/pull/2100)) | `open {parentCallId, context}` → `{gateId}`; `invoke {gateId, call}` → `{result}`; `close {gateId}` → `{effects}` | Child calls, admission, child facts; the run closes the gate to word its result, and the owner closes it again at parent settlement with the effects merged once |
| ToolBinding ([#2028](https://github.com/arcships/dim/issues/2028), target, pending [#2100](https://github.com/arcships/dim/pull/2100)) | `adopted {requestId}` → `{adoption, tools}` | The request's pinned declarations for `tools.list`, `tools.describe`, `tools.call` |
| Session registries ([#2026](https://github.com/arcships/dim/issues/2026), target, pending [#2103](https://github.com/arcships/dim/pull/2103)) | `toolRegistry.register` | The `codemode` contribution |
| (none) | — | The v4 gate is the mount exclusion in the Session's creation record; no version read and no service call |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `codemode` | The model submits one program | `{code}` | the program's returned value as JSON text, or a terminal failure text, with the confirmed effects | The program is interrupted: a synchronous loop stops, a pending child is cancelled and new children are refused | `invalid_input` when the arguments or the code size are refused, returned as an error result |
| `codeModeLine.run` | Run one program on this line | `{outerCallId, code, context}` | `ToolResult` | Interrupts the program and closes the gate; committed children settle through ToolExecutor | `invalid`, `closed`, `cancelled` as the service error `{code, message}`; a failed program is the result's text. The child gate's `interrupted`, `no_batch` and `gate_closed` are ToolExecutor's kinds, listed below for the program's handling, not this operation's errors |
| `codeMode.with_slot` | Run one program inside a process slot | `{work}` as a direct callback parameter, plus `caller: &Ctx` | the work's reply `T` | The work is cancelled and joined; the slot is released | `closed` as the service error `{code, message}`; a cancelled work is the callback's own result |

The program's own failure is a result, not an operation error: the reply carries the failure text, the warning that child calls may have completed, and the confirmed effects. `invalid` is a code body over the limit; `closed` is the owner unloading; `cancelled` is the caller's cancellation before or during the run. All three are the service error `{code, message}`; the child invocation's `gate_closed`/`interrupted`/`no_batch` are ToolExecutor's errors, which the run turns into a program failure or a cancellation.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `codemode` | The handler resolves the line's run owner and calls it; one result for the outer call | The program is interrupted; committed children settle through ToolExecutor; the result names the cancellation | The run is cancelled by the line's close; the outer call is settled by ToolExecutor | Reload settles the outer call as started but unfinished with the effect-may-have-happened warning; no child is replayed | ToolExecutor: the outer and child facts; the run owner: the QuickJS context |
| `codeModeLine.run` | Open the gate, evaluate the program, close the gate for the confirmed effects, answer the program's result | Interrupt, refuse new children; committed children record their own cancellation; the early close still answers the effects confirmed before it | The line's close cancels the run before the outer result | A closing or reloading line ends the run; ToolExecutor settles the gate, the child facts and the parent result | the run owner: the QuickJS context; ToolExecutor: the gate, every child fact, the effects and the outer result |
| `codeMode.with_slot` | Take a slot, run the work, release the slot | The work is cancelled and joined; the slot is released | The row's unload releases the slot it holds | New slots are refused once the process is unloading; held slots are released | the quota owner |

The program's memory is bounded by QuickJS's own limit, its time by the wall-clock interrupt, and its output and each child's serialization by the configured byte limits. A child the program never awaits is still committed and joined before the outer result. Code Mode closes the gate once, after the program ends, to word the result with the confirmed effects; ToolExecutor closes it again at parent settlement without effect, and the same effects appear once in the parent result.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the program: it exceeds the code byte limit |
| `closed` | The line's run owner, the executor or the process quota is unloading; the next generation serves the next call |
| `interrupted` (ToolExecutor's `open`) | The turn was interrupted before the gate existed; the run answers a cancellation result |
| `no_batch` (ToolExecutor's `open`) | The outer call is not in the current batch; a caller bug: the run answers a failure result naming it |
| `gate_closed` (ToolExecutor's `invoke`/`close`) | The parent call settled or the batch was interrupted before the run closed the gate; the program's next child call fails and the run ends with the effects the parent already carries |
| `closed` (ToolExecutor) | The executor is unloading; the run answers a cancellation result |
| `cancelled` | The call stopped where the caller cancelled; committed children are reported in the effects |
| a failed program (a result, not an error) | The model reads the failure text and the warning; it checks the effects before retrying |

## Exceptions with reasons

- One QuickJS context per call, in the line's scope: the program is model-supplied code, and the line is the smallest scope that still holds the request's pinned tools.
- The child gate stays inside ToolExecutor: admission, child facts, effects and the terminal result are that owner's; Code Mode opens the gate, invokes by id and closes it once for its result text, and the owner's settlement remains the backstop.
- The program's failure is data on the tool result, not an operation error: the model must read it and decide, and the outer result is the only model-visible outcome.
- A recursive `codemode` call is refused by the pinned set, not by a second check: the program's tool list excludes the tool itself.
- The v4 client gate is the entry's declaration: v1–v3 clients could not surface a child `ask_user` or a child approval.

## Verification

Flow tests through the public interfaces:

- A program calls a pinned tool and the outer result carries its value; a child result reaches the program whole.
- A child `ask_user` waits and returns the answer; a child approval names the child call; a denied child is a catchable program error.
- A completed child's effect survives the parent's reload; an open child fails when ToolExecutor reloads; no child is replayed.
- A synchronous loop is interrupted; a never-settling promise times out; the call budget, the parallel limit, the code limit and the single-result limit are enforced.
- Two Sessions with `main` lines keep their own runs; a third program waits for a slot and is released by cancellation.
- The `codemode` tool is declared only to v4 Sessions, and a program cannot call `codemode`.

## Supersedes

- This document's earlier shape sections: the process runner map, the runner trait and the executor wrapper are replaced by `codeMode.with_slot` and `codeModeLine.run`. The tool contract, the child batches, approval and the client boundary remain as described in [child tool batches](../appserver/child-tool-batches.md) and in this document's contract sections.
- [runtime-owner-operations.md](../contracts/runtime-owner-operations.md): the Code Mode row that reads `toolBinding.definitions`; the run reads `toolBinding.adopted`.

## Client-visible changes

None. The `codemode` tool keeps its name, description, schema, risk and v4 gate; child batches and the outer result are unchanged; the gate is still opened and closed by ToolExecutor.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | The process part owns the bounds and the slots and registers the two parts; the Session part registers the tool; the branch part runs the program. No part folds state, so none is a Capability. |
| §2 scopes | The bounds and slots are process state; the declaration is Session state; the QuickJS context is the line's. |
| §3 orthogonality | Code Mode reads the pinned declarations and invokes tools through ToolExecutor; it returns no other owner's data. |
| §4 rule 1 (commands) | Child calls are ToolExecutor's commands; Code Mode writes no Timeline fact. |
| §4 rule 2 (handover) | The gate is ToolExecutor's; Code Mode holds it only for the run and closes it before answering. |
| §4 rule 3 (before callbacks) | None registered. |
| §4 rule 4 (no business meaning in core) | The tool, the bounds and the program text are Code Mode's; ToolExecutor's nested operation names no Code Mode type. |
| §4 rule 5 (batches) | The outer batch stays current; child batches do not take a model batch slot. |
| §4 rule 6 (context) | Only the outer result is model-visible; child results reach the program. |
| §4 rule 7 (public events and services) | The registry, the line's owners and ToolExecutor's operations; no handler or executor internals. |
| §4 rule 8 (requirements) | The line-id provider is the open one; #2028's contract covers the nested invocation. |
| §4 rule 9 (facts) | Child calls and their terminal results are facts; effects travel on the child result and the outer result. |
| §7 values, ids and callbacks | The run takes data and a callback; the quota takes the caller's work as a callback; the gate is an id for `invoke` and `close`, and the owner closes it at settlement; no object crosses. |
| §7 no proxy lookup | The entry does not hold a run table; it asks the call's line. |
| §7 one interface | One tool contribution, one run operation, one quota operation; no adapter, no runner trait. |
| §7 cross-language | The tool, the run and the quota are data and callbacks; the program's bridge is `tools.list/describe/call`. |
| §7 one change | Deleting the process runner table, the runner trait and the executor wrapper happens in the same change that adds the quota operation and the line's run operation. |
| ADR 002 D1 | Slots are process state; the run is line state; the declaration is Session state. |
| ADR 027 D1, D4 | The tool handler reaches `codeModeLine` and the line's ToolExecutor by the line id on its execution context; the resolution is not a dependency and is not kept across calls. |
| ADR 027 D3 | `codeModeLine` and ToolExecutor are branch data faces; no registration point or lifecycle service is resolved by id. |
| ADR 002 D2 | No wiring plugin: both operations are on their owners' types; the mechanism resolves the line. |
| ADR 002 D3 | Dependencies point from Code Mode to the registry, the mechanism and ToolExecutor. |
| ADR 002 D4 | The bounds are structural configuration; no business setting is read. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

Both requirements below land in the id routing mechanism [#2127](https://github.com/arcships/dim/issues/2127), owned by the dim runtime; #2024's contract (#2092) states the line-id operations that mechanism serves.

- [TimelineTree #2024](https://github.com/arcships/dim/issues/2024) (target, pending [#2092](https://github.com/arcships/dim/pull/2092)): the cross-cutting decision names this owner's contract as the place that fixes who provides the line-id operations to Session parts; the mechanism that serves them is [#2127](https://github.com/arcships/dim/issues/2127). Code Mode needs two: a Session-registered tool handler resolving `codeModeLine` by the line id on its execution context, and the run resolving the line's ToolExecutor. SessionScope's contract has no such operation.
- [The branch tool chain #2028](https://github.com/arcships/dim/issues/2028): none open; `open`/`invoke` and `toolBinding.adopted` are what this contract calls ([#2100](https://github.com/arcships/dim/pull/2100)).
- The line's timeline owner (runtime): child `step/tool-call` and `step/tool-result` batches carry `parent_call_id`; the v1–v3 projection omits them.

## Binding appendix

The process part provides `codeMode`; the branch part provides `codeModeLine`; the Session part registers the tool. Both names are set with `#[rutis::service(name = ...)]` on the owner type. Request, reply and error types are data; `cancelled` is the binding's cancellation argument.

```rust
use rutis::host::BoxFuture;

#[rutis::service(name = "codeMode")]
impl ProgramSlots {
    /// Run `work` while holding one process program slot; `caller` binds the
    /// slot to the calling row, so the owner releases it at that row's unload.
    pub async fn with_slot<T: Send>(
        &self,
        caller: &Ctx,
        work: WorkFn<T>,
        cancelled: CancelledFn,
    ) -> Result<T, ServiceError>;
}

/// The service error shape of every data face: `{code, message, data?}`.
#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct ServiceError {
    pub code: String,                 // "closed"
    pub message: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgram {
    pub outer_call_id: String,
    pub code: String,
    pub context: ToolExecutionContext,
}

/// The quota's work callback: it gets the caller's cancellation and answers the
/// work's own reply or a service error that keeps the caller's code.
pub type WorkFn<T> = Arc<
    dyn Fn(CancelledFn) -> BoxFuture<'static, Result<T, ServiceError>> + Send + Sync,
>;

#[rutis::service(name = "codeModeLine")]
impl CodeModeLine {
    /// Run one program in this line's scope; the gate is ToolExecutor's.
    pub async fn run(
        &self,
        caller: &Ctx,
        request: RunProgram,
        cancelled: CancelledFn,
    ) -> Result<ToolResult, RunError>;
}

/// `RunError` is the same `{code, message}` shape: `invalid`, `closed`, `cancelled`.
```

`ToolExecutionContext` and `ToolResult` are `dim_agent_types`' types; `ServiceError` is the same `{code, message}` shape every data face uses; the child operations are ToolExecutor's ([#2028](https://github.com/arcships/dim/issues/2028)).
