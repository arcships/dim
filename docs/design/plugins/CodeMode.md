# Code Mode: bounded QuickJS tool orchestration

Status: Draft. Phase: P3. Delivery status is tracked in [#1276](https://github.com/arcships/dim/issues/1276), not here.
Language: native Rust (`rquickjs`). The plugin is `crates/dim-codemode/`; it is not a TypeScript
package and runs no Node or TypeScript plugin runtime. Scope: process, Session and branch
([ADR 002](../../adr/002-plugin-scope.md)).

Code Mode lets a model submit one bounded program that invokes the request's adopted Session tools,
collects the results and returns one ordinary tool result. A program's tool calls are child tool
calls on the branch Timeline, not a second model batch. The design reuses `step/tool-call` and
`step/tool-result`; the only event-envelope addition is an optional `parent_call_id`.

The host installs the plugin with `dim_codemode::install`. The App Server product profile may
install the plugin, but only a protocol v4 Session is offered the `codemode` tool; v1–v3
connections do not receive child events and cannot start a Code Mode program. The client boundary
is specified in [child tool batches](../appserver/child-tool-batches.md). A host that does not want
the tool omits the call.

Issue: [#1276](https://github.com/arcships/dim/issues/1276).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md), [plugin contract](../plugin-contract.md).

## 1. Scope and ownership

### 1.1 Parts

| Part | Scope | Owns |
| --- | --- | --- |
| `CodeMode` | Process | The structural `CodeModeConfig` bounds, the process `CodeModeRuns` map from `(session id, branch id)` to its runner, the program semaphore, and the registrations of the Session and branch parts. |
| `CodeModeSession` | Session | The `codemode` tool declaration and handler. The handler resolves the caller's `(session id, branch id)` in `CodeModeRuns`; it does not read Session state to find a branch. |
| `CodeModeBranch` | branch | The one-shot QuickJS program, the `tools` bridge, the parent gate use and the confirmed child effects copied onto the outer result. |
| ToolExecutor nested invocation | branch, mechanism owner | The pinned `ToolBinding` lookup, the child `step/tool-call` command, nested execution, Approval admission and the child terminal result. This is not Code Mode business state. |
| QuickJS runtime | branch | The execution boundary for model code. It receives only the serializable `tools` bridge. |

`CodeMode` is a process plugin, not a `Feature`: it provides `CodeModeRuns` and registers the two
parts. `CodeModeSession` and `CodeModeBranch` are plain parts. `CodeModeBranch` is not a Capability
in this delivery: the part owns runtime-only execution state and the parent gate use, while
ToolExecutor owns the child `step/tool-call`/`step/tool-result` facts.

The bounds are structural `CodeModeConfig` values. A user `codemode` Settings source is deferred; no
part invents defaults from another source.

### 1.2 Goals

- Allow one model tool call to run a bounded asynchronous program.
- Let the program invoke tools from the request's fixed `ToolBinding` snapshot.
- Keep every child invocation under the invoked tool's Permission and Approval policy.
- Record every child invocation as a child `step/tool-call` batch and one terminal
  `step/tool-result`, with an explicit parent call.
- Return one bounded outer `ToolResult`; child results do not become model messages.
- Copy confirmed child effects onto the outer result for the existing compaction digest.
- Never replay a child side effect after cancellation, timeout or reload.

### 1.3 Non-goals

| Not done here | Why |
| --- | --- |
| Persistent REPL, cells or cross-turn JavaScript state | That is durable execution state and belongs in a separate design or Workflow. |
| A separate new event type for child calls | Child calls reuse `step/tool-call`/`step/tool-result`; `parent_call_id` carries the relation. |
| A separate OS-confined native host process | The first delivery runs QuickJS in the daemon process with memory, time and interrupt limits; process-level hardening is future work. |
| Direct access to ToolExecutor internals, handlers, ToolHost, file descriptors or host objects | Those would bypass the pinned binding and admission boundaries. |
| An implicit grant of all child permissions after approving `codemode` | Each invoked tool remains independently subject to Permission and Approval. |
| Recursive `codemode` calls | The allowed tool set excludes `codemode`; the first delivery has no nested Code Mode. |
| TypeScript type stripping | The first delivery evaluates JavaScript; a TypeScript front end is future work. |

### 1.4 Tool exposure and eligibility

`codemode` is declared beside the other adopted tools. The bridge exposes every ordinary tool in the
outer request's pinned `ToolBinding`, including eligible built-in, extension and MCP tools.
`codemode` itself is excluded. `tools.list` and `tools.describe` are projections of the same pinned
snapshot; `tools.call` rejects a name absent from it. A malformed or unknown call is a local program
error and writes no child Timeline fact.

## 2. Own contract

### 2.1 The `codemode` tool

`CodeModeSession` registers one tool named `codemode`:

```json
{
  "code": "const listed = await tools.list(); return listed"
}
```

| Field | Meaning |
| --- | --- |
| `code` | Raw JavaScript async function body. It is rejected when absent, not a string or larger than `max_code_bytes`. |

The tool has no model-facing timeout, call budget or concurrency parameter. Those are host
bounds; the model cannot raise or lower them.

`CodeModeConfig` supplies the hard bounds:

```text
max_code_bytes          256 KiB
max_runtime_ms          600 s
max_calls               4096
max_parallel_calls      16
max_memory_bytes        1024 MiB
max_programs            2
max_output_bytes        16 MiB
max_single_result_bytes 4 MiB
```

`max_programs` bounds concurrent programs in one process. `CodeModeRuns` holds the program
semaphore; aggregate QuickJS memory is bounded by `max_programs * max_memory_bytes`. A program
waiting for a slot is cancelled by the outer call token. `max_parallel_calls` bounds child tool
tasks, and `max_single_result_bytes` bounds one child result before its full JSON string is
materialized. These bounds are host policy, not tool parameters and not prompt text.

The outer tool is declared with risk `session`. The outer call enters the normal ToolExecutor
admission path under the Session's permission preset. Approving the outer call does **not** approve
child calls: each child uses the invoked tool's own risk.

The model-visible description states only the contract the model must act on: the raw JavaScript
function body, top-level `await`/`return`, the `tools.list`/`tools.describe`/`tools.call` shapes,
`args` as an object, the whole child `ToolResult` (`content` and `structuredContent` together,
with the guidance to return `content` unless a program needs the structured fields),
`ToolCallError`, the exclusion of `codemode`, the absent host APIs, the JSON return value, and that
child results do not enter model context. Host bounds, engine name, memory, program count and
error-code tables are not prompt text.

The outer result is a normal `ToolResult`. `content` is the program's returned value serialized as
JSON, or a stable terminal message on failure. `effects`, when present, are the confirmed effects of
child `ToolResult`s in child sequence order, including effects from an error result. A generated
timeout or failure result carries the effects that completed before it and warns that child calls
may have completed.

### 2.2 QuickJS bridge

The program sees only:

```text
tools.list() -> [{name, description}]
tools.describe(name) -> declaration | undefined
tools.call(name, arguments) -> the whole child ToolResult
```

`arguments` is a JSON object. `tools.call` returns the child `ToolResult` as the program's value:
`content` (a string or content blocks) and `structuredContent` are both carried, so a program reads
the same content the model would read and the structured data the tool published. A child result
with `isError` throws `ToolCallError` with `code`, `message` and optional `data`. The error object is
data, not a second result shape.

The context has no `process`, environment, filesystem, network, timer, console, child-process or
module loader. The host bridge is passed as the function argument; no host object is exposed as a
global. `tools.call` rejects a non-object argument and every name outside the filtered pinned set,
so `codemode` cannot recurse.

The runtime enforces `max_code_bytes`, `max_runtime_ms` (a Rust `timeout_at` around the program and
a QuickJS interrupt handler), `max_memory_bytes` (QuickJS memory limit), `max_calls`,
`max_parallel_calls`, `max_single_result_bytes` and `max_output_bytes`. A synchronous loop is
interrupted by the QuickJS handler; cancellation also sets the interrupt and wins over the program
await. These bounds are not written into the prompt.

## 3. Child calls on the Timeline

### 3.1 Event model

The Timeline keeps one call event type:

- A top-level model request writes `step/tool-call` without `parent_call_id`.
- A child call writes `step/tool-call` with `parent_call_id` set to the outer `ToolCall.id`.
- The child result writes `step/tool-result` with `parent_call_id` set to the same outer
  `ToolCall.id`; `call_event_id` points to the child `step/tool-call` event.
- A grandchild call writes `step/tool-call` with `parent_call_id` set to the child `ToolCall.id`.

`parent_call_id` is an optional top-level property on `AgentEvent`, not a field inside
`ToolCallData`. The retrieval layer filters child batches without decoding `ToolCallData`; TUI and
App Server read the same property.

Events produced while one child call executes carry `parent_call_id` set to the child
`ToolCall.id`. This includes asynchronous callbacks, background `task/*` results and child
`approval/*` events. A child `ask_user` call is itself a `step/tool-call`, so the child
`ask_user` batch carries the outer call id like every other child batch. The rule is:

- For `step/tool-call` and its `step/tool-result`, `parent_call_id` is the call that spawned the
  batch: the outer call for a child batch, the child call for a grandchild batch.
- For every other event produced by one call, `parent_call_id` is that call's `ToolCall.id`.

The sequence is the commit order; `parent_call_id` is the structure. No reader infers a parent from
the open-batch order, so parallel children and grandchildren stay unambiguous.

### 3.2 Normal sequence

```text
step/tool-call (batch A, no parent_call_id)
  calls: [codemode c1]
  step/tool-call (batch B, parent_call_id = c1)
    calls: [X c2]
    step/tool-result (batch B result, parent_call_id = c1)
      results: [c2]
  step/tool-result (batch A result, no parent_call_id)
    results: [c1, effects from c2]
```

1. Loop writes batch A with `codemode` c1. ToolExecutor opens batch A.
2. The `codemode` handler starts the QuickJS program and opens the parent gate for c1.
3. The program calls `tools.call("X")`. The nested invocation service writes batch B with
   `parent_call_id = c1` and `calls: [c2]`.
4. ToolExecutor runs c2 through the pinned binding, Permission and Approval.
5. ToolExecutor writes batch B's `step/tool-result` with `call_event_id = B`,
   `parent_call_id = c1` and c2's `ToolResult`.
6. The program receives c2's result. Batch B is terminal.
7. The parent gate closes after every committed child is terminal. ToolExecutor writes batch A's
   `step/tool-result` with c1's result and the confirmed child effects.
8. Loop continues with the next model request.

Batch A stays open while child batches B, C and so on are open. The ordinary batch slot belongs to
batch A; child batches do not take a second model batch slot.

### 3.3 Parallel and nested children

- Batch A can open child batches B (c2) and C (c3) at the same time.
- Batch B and batch C both carry `parent_call_id = c1`.
- Child call c2 can open grandchild batch D (c4) with `parent_call_id = c2`.
- The parent gate for c1 waits for c2, c3 and their descendants.
- A child result closes the child batch it answers; a grandchild result closes the grandchild
  batch.
- ToolExecutor never treats a batch with `parent_call_id` as a model batch.

### 3.4 Parent gate and effects

The parent gate is a branch-scoped contract of the nested invocation service:

```text
open(parent_call_id)
  -> invoke(...)          // writes a child step/tool-call and returns its result
  -> close(reason)        // rejects new children and waits for every committed child
```

`close` runs for normal return, program failure, host timeout and cancellation. On reload,
ToolExecutor's rebuild applies the reload rule in §3.6 instead. `close` stops new admission, cancels
children that have not started or that the program no longer awaits, and waits up to a five-second
grace for a terminal `step/tool-result` for every committed child before the outer result is
written. A handler that ignores the cancellation token past the grace is outside the contract; its
terminal fact may settle after the outer result. A child waiting for Approval is cancelled through
the existing Approval contract.

Child effects are collected from terminal child `ToolResult`s in child sequence order, including
effects from an error result, and appended to the outer `ToolResult.effects`. The gate records a
child's effects when its task writes the terminal result, so an unawaited child that completes
before `close` still contributes effects. `max_calls` bounds the number of child results;
`max_single_result_bytes` bounds one child serialization; each invoked tool remains responsible for
its own result bounds. ToolExecutor records the returned outer
result unchanged under the Session boundary contract.

### 3.5 Cancellation and timeout

- The parent gate closes admission.
- A child that has not started settles with an error `ToolResult` whose text names cancelled or not
  executed; the design adds no separate status field.
- A started child receives the child cancellation token; ToolExecutor writes its terminal result.
- The outer result follows the parent gate; confirmed child effects are included.
- Every child call uses a per-program child `CancellationToken`; the token is cancelled whenever the
  program returns, times out or is cancelled. A dropped Approval admission is removed and writes
  `approval/cancelled`; a pending question is settled with the same child token. Approval admission
  needs a drop guard: dropping the admission future removes the pending request and writes
  `approval/cancelled`.
- A call the program starts and never awaits is still a committed child. The parent gate joins the
  child; the program cannot finish with an unjoined child.

### 3.6 Reload

The reload rule is stated once, here:

- A child `step/tool-call` with a matching `step/tool-result` uses the written result and effects.
- A child `step/tool-call` without a terminal result counts as started but not finished. ToolExecutor
  writes an error `ToolResult` for the child and no confirmed effects.
- The outer `codemode` call counts as started but not finished when batch A has no terminal result.
  ToolExecutor writes a failure result with an effect-may-have-happened warning and the confirmed
  effects from terminal child results.
- No child is replayed after reload.
- The outer result is settled by ToolExecutor; CodeModeBranch does not write a competing outer
  result.

The existing `ToolExecutor` rebuild settles the outer batch. The child-batch work extends the same
rebuild to the child batches and their terminal results.

## 4. Questions and TUI

- A child `ask_user` call is a normal `step/tool-call` batch with `parent_call_id`. The batch is
  visible only to v4 clients; v1–v3 clients receive only the outer result and are not offered the
  `codemode` tool.
- `session.question.pending` queries every `step/tool-call` with an `ask_user` call and no
  `step/tool-result` whose `call_event_id` is that batch. Top-level batches and child batches use
  the same query.
- The pending reply returns the batch event id, `request_id` and each child call id.
- `session.question.respond` uses `request_id` and the child call id; the waiter key is unchanged.
- ToolExecutor writes the answer as the child batch's `step/tool-result`.
- TUI displays events in `sequence` order. A v4 TUI groups the child batch under
  `parent_call_id`; a child event whose parent is outside the loaded window is hidden, never
  promoted to a top-level row.
- A child batch may hold one or more calls; parallel child calls may be separate child batches. The
  pending query returns each batch; TUI queues `ask_user` calls in batch order, as TUI already does
  for a model batch.
- TUI must not treat a child batch as a model request. `parent_call_id` is the distinction.

## 5. Branch and Hooks

- A child `branch` call is in a child `step/tool-call` batch. `BranchSource::Tool.call_id` is the
  child call id; the child batch exists on the Timeline.
- Hooks read every `step/tool-call` call, including batches with `parent_call_id`, and report the
  child call id as `tool_use_id`. Recall is not extended to child calls in this delivery; its
  current model tool-use projection is unchanged, and the child-grouping claim is removed.
- Query results and hooks payloads must not filter child batches out.
- `branch` default `at` keeps the existing request cut: the branch tool finds the outer request's
  `step/tool-use` and the preceding `step/model-start`. The child batch provides the source call,
  not the cut.

## 6. Approval and Permission

Two authorization decisions exist:

```text
outer codemode call
  -> Permission / Approval for codemode's declared `session` risk

child tools.call(name)
  -> Permission / Approval for name's declared risk
```

Child approvals reuse `approval/request`, `approval/respond` and cancellation.
`ApprovalRequestData.source` is `BranchSource::Tool { call_id: childCallId, turn_id, request_id }`.
The child call id is present in the child `step/tool-call`, so v4 clients and Hooks resolve the
source. `approval_id` remains the only key accepted by the response operation. A response authorizes
only that child call; a response never authorizes a sibling, a future call or the outer `codemode`
call.

A child approval and background result carry `parent_call_id = childCallId` on the event envelope.
A child `ask_user` batch carries `parent_call_id = outerCallId`, like every child `step/tool-call`.
The outer approval (when the outer call asks) has no `parent_call_id`.

## 7. Storage and client compatibility

- `parent_call_id` is an optional top-level property on `AgentEvent`; `ToolCallData` and
  `ToolResultData` keep their current shapes.
- The native store advances by one format step for the new optional event property and the child
  batch semantics. The step number is the next free number at implementation time; the marker table
  `CHECK` and its single row advance, while business tables, columns, indexes and rows do not.
- An older binary refuses the newer marker, preserves the store byte-for-byte and never downgrades.
- v1, v2 and v3 clients never receive child `step/tool-call`/`step/tool-result` events. The App
  Server filters them at the wire boundary; their event allowlists and outer tool projection are
  unchanged. They are also not offered the `codemode` tool, because a child `ask_user` or child
  approval could not be surfaced.
- v4 clients receive child batches with `parent_call_id`. They must group the child under its
  parent or hide it; rich child detail is optional.
- The upgrade/downgrade fixture includes a top-level batch, a child batch, a child result and a
  child background result. The v3 projection fixture proves the child events are absent and the
  outer events still reach the client; the v4 fixture proves the parent property is present.

## 8. Dependencies

| Dependency | Scope | Consumer | Purpose |
| --- | --- | --- | --- |
| `partRegistry` | Process | `CodeMode` | Register `CodeModeSession` and `CodeModeBranch`. |
| `CodeModeRuns` | Process | `CodeModeSession`, `CodeModeBranch` | Resolve the calling Session and branch's runner; the process plugin provides it. |
| `toolRegistry` | Session | `CodeModeSession` | Register the `codemode` tool. |
| `ToolExecutor` | branch | `CodeModeBranch` | Read the pinned declarations, submit child `step/tool-call` batches, run child calls and join the parent gate. |
| `TimelineIdentity` | branch | `CodeModeBranch` | The calling Session id and branch id. |
| `rquickjs` | library | `CodeModeBranch` | The QuickJS runtime and the `tools` bridge. |

The plugin does not depend on `ToolHost`, a handler registry, a second event bus, direct Session
facade access or a Node/TypeScript runtime.

## 9. Requirements raised to mechanism owners

### R1. ToolExecutor child batches and parent gate

| Field | Content |
| --- | --- |
| Problem | A branch part must run an adopted tool while its own outer tool call is current. Calling a handler or `ToolHost` directly bypasses the pinned binding and Permission. A second top-level `step/tool-call` makes Loop and ToolExecutor treat the child as a model batch. |
| Existing mechanisms considered | `ToolBinding.handler` alone bypasses Approval; a second top-level `step/tool-call` fails with "concurrent tool batches"; a Session-side registry read is not the request snapshot. |
| Generic capability | A branch-scoped nested invocation service that writes child `step/tool-call` batches with `parent_call_id`, executes the child calls through the request-pinned binding, Permission and Approval, writes child `step/tool-result` events, and exposes `open`/`invoke`/`close` to the caller. The service refuses a call when the outer batch is closed, interrupted or not the current batch; a closed or interrupted executor never exposes the nested surface. |
| Sequence, batch, interruption and reload | Child batches do not take a model batch slot. The parent gate closes admission and waits for every committed child before the outer result. Cancellation uses a per-program child token; Approval admission has a drop guard that removes a pending request and writes `approval/cancelled`. Reload uses the rule in §3.6: terminal child results are used; started-but-unfinished children fail; the outer call fails with the effect-may-have-happened warning and confirmed child effects. |
| Owner and issue | Dim runtime / ToolExecutor mechanism owner; [#1276](https://github.com/arcships/dim/issues/1276). |

### R2. QuickJS execution boundary

| Field | Content |
| --- | --- |
| Problem | A model-supplied program cannot be safely run with `eval`, Node or the trusted plugin process. The engine must have no host capability except the tool bridge. |
| Existing mechanisms considered | The `rquickjs` native QuickJS binding, `AsyncRuntime`/`AsyncContext`, `Promise`-wrapped host functions, `set_memory_limit` and `set_interrupt_handler`. |
| Generic capability | One fresh QuickJS context per call with only `tools.list`, `tools.describe` and `tools.call`, a hard memory limit, a wall-clock interrupt and a child cancellation token. A separate native host process is a future hardening. |
| Sequence, batch, interruption and reload | The context is dropped after the program's promise settles. A dropped child token settles a child Approval or question. Reload settles the outer call through ToolExecutor; no child is replayed. |
| Owner and issue | Runtime/host mechanism owner; [#1276](https://github.com/arcships/dim/issues/1276). |

### R3. Questions child batches

| Field | Content |
| --- | --- |
| Problem | The TUI question path assumes every `step/tool-call` with `ask_user` is a model batch. A child `ask_user` call is a `step/tool-call` batch with `parent_call_id`; the current TUI does not receive or group child batches, so the prompt cannot be shown or answered reliably. |
| Existing mechanisms considered | The existing pending query already selects `step/tool-call` rows; the waiter table and `session.question.respond` key already use `request_id` + `call_id`. |
| Generic capability | The pending query includes child batches; the reply returns the child call id; TUI receives child batches, displays them in sequence order and queues `ask_user` calls. |
| Sequence, batch, interruption and reload | The child batch is visible only to v4 clients, and only v4 Sessions are offered `codemode`. The waiter key is `(branch, request, child call id)`. The child result closes the batch. Reload settles a waiting child through ToolExecutor's failure rule. |
| Owner and issue | Questions owner; [#1276](https://github.com/arcships/dim/issues/1276). |

### R4. Branch and Hooks child batches

| Field | Content |
| --- | --- |
| Problem | Branch source lookup and Hooks `tool_use_id` assume every call is in a top-level model batch. Recall keeps its separate model tool-use projection. |
| Existing mechanisms considered | The child call is a normal `step/tool-call` batch with `parent_call_id`; the call id is unchanged. |
| Generic capability | Branch and Hooks read every `step/tool-call`, including child batches; `BranchSource::Tool.call_id` and `tool_use_id` use the child call id. Recall keeps its model tool-use projection; child Recall is a separate design. |
| Sequence, batch, interruption and reload | Child batches are terminal before the outer result; reload uses the child result written by ToolExecutor. |
| Owner and issue | Branch and Hooks owners; [#1276](https://github.com/arcships/dim/issues/1276). Recall child grouping is a separate design. |

### R5. Event envelope, storage and v1 projection

| Field | Content |
| --- | --- |
| Problem | `step/tool-call` and `step/tool-result` need a parent relation; the retrieval layer must distinguish child batches without decoding `ToolCallData`; v1 and v2 projections must not expose the new envelope property. |
| Existing mechanisms considered | An optional top-level `AgentEvent.parent_call_id`; the existing `step/tool-call`/`step/tool-result` shapes. |
| Generic capability | The event envelope carries optional `parent_call_id`; the native store advances one format step; v1–v3 projections omit parented events at the wire boundary; v4 clients and TUI read the property. |
| Sequence, batch, interruption and reload | The property is written at commit time with the event; the upgrade/downgrade fixture covers a top-level batch, a child batch, a child result and a child background result. |
| Owner and issue | Timeline/event and App Server owners; [#1276](https://github.com/arcships/dim/issues/1276). |

## 10. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Capability boundary | `CodeModeBranch` is a branch part with runtime-only execution state, not a Capability in this delivery. `CodeModeSession` only registers a tool; `CodeMode` only owns process configuration and registrations. |
| §4.1 split by state owner | Process owns `CodeModeRuns`, the program semaphore and the bounds; Session owns the tool declaration; Branch owns the one-shot QuickJS execution and parent gate use; ToolExecutor owns child batches, admission and terminal facts. |
| §4.2 command and handoff | A child `step/tool-call` with `parent_call_id` is the durable handoff command. The parent gate closes admission and joins every committed child before the outer result. |
| §4.3 handlers register work and return | The `codemode` handler awaits the QuickJS program and the parent gate; the branch owns runtime waiters and settles them before returning. |
| §4.4 core mechanism has no business meaning | The nested invocation service and `parent_call_id` are generic; the contract names no Code Mode type. |
| §4.5 atomic batches and ordered dispatch | Child batches are committed and dispatched in sequence. The outer batch stays current; child batches do not take the model batch slot; the outer result is committed once. |
| §4.6 context boundary | Child results never enter context. Only the outer `step/tool-result` is model-facing; a value the model needs is in the outer `content`. |
| §4.7 public interaction | The plugin uses `ToolExecutor`, `ToolRegistry`, `CodeModeRuns` and `rquickjs`; it receives no handler or ToolExecutor internals. |
| §4.8 infrastructure requirements | R1–R5 state the existing mechanisms, the generic additions, cancellation/reload behavior and the owners. |
| §4.9 facts in Timeline | Every child call has a durable `step/tool-call` command and a terminal `step/tool-result`; child effects travel on the child result and the outer result. |
| ADR 002 D1 | Process `CodeMode`, Session tool, Branch execution and ToolExecutor each own their own state. |
| ADR 002 D2 | No wiring plugin, second bus or lifecycle manager is added. |
| ADR 002 D3 | Dependencies point from Code Mode to existing core contracts and ToolExecutor; core does not depend on the optional crate. |
| ADR 002 D4 | Bounds are structural `CodeModeConfig`; a user Settings source is deferred. |
| ADR 002 D5 | The child-batch, nested invocation and QuickJS contracts are Rust data/error shapes; the binding does not leak into the design. |

Ablation: this design keeps the existing `step/tool-call`/`step/tool-result` shapes, adds only the
optional envelope property, reuses the existing Approval and Questions waiters, and gives child
effects one path: the child result. A separate child event type, a second tool registry, a
persistent REPL, a separate host process and a user settings source are not part of this delivery.

## 11. Binding appendix

The following is semantic pseudocode, not a committed spelling.

```text
CodeMode::apply(ctx):
  runs = Arc<CodeModeRuns>
  ctx.provide(runs)
  partRegistry.register(Session, "CodeModeSession", |names| CodeModeSession::new(names))
  partRegistry.register(Branch,  "CodeModeBranch",  |names| CodeModeBranch::new(names, config))

CodeModeSession::apply(ctx):
  session_id = sessionIdentity.session_id
  tools.register(declaration = {
    name: "codemode",
    risk: "session",
    inputSchema: { code }
  }, handler = |call, context, cancel|:
    runner = runs.get(session_id, context.branch_id) or error
    runner.run(call.id, call.code, context, cancel))

CodeModeBranch::run(outer_call_id, code, context, cancel):
  child_cancel = cancel.child_token()
  program_slot = runs.acquire_program(child_cancel) or error
  runtime = AsyncRuntime::new(); runtime.set_memory_limit(max)
  runtime.set_interrupt_handler(deadline || child_cancel)
  quickjs = AsyncContext::full(runtime)
  gate = toolExecutor.open(outer_call_id)
  result = try:
    quickjs.async_with(|ctx|:
      install tools.list / tools.describe from executor.definitions(context.request_id)
      install tools.call = Async(|name, args| gate.invoke(ToolCall{name, args}, context, child_cancel))
      promise = ctx.eval(async function body)
      return promise.into_future())
  finally:
    child_cancel.cancel()
    gate.close(reason)
    drop(program_slot)
  return ToolResult { content: JSON(result), effects: collected_effects }
```

The host adds the process row with `dim_codemode::install`; `install_required` also makes both
parts required for a Session that must publish only after the tool is registered. The product
profile may install the plugin by default once the v4 protocol, TUI/web grouping and the
`AgentLoop` rebuild filter in [child tool batches](../appserver/child-tool-batches.md) land. The
Session gate keeps `codemode` out of v3 connections even when the plugin is installed.
