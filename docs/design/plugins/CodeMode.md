# Code Mode: bounded programmatic tool orchestration

Status: Draft. Delivery status is tracked in the issues, not here.

Scope: Session and branch.
Issue: [#1276](https://github.com/arcships/dim/issues/1276).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md), [plugin contract](../plugin-contract.md).

Code Mode lets a model submit one bounded program that invokes adopted Session tools, collects the
results and returns one ordinary tool result. It is a tool orchestration capability, not a second
agent loop, a persistent REPL or a Workflow engine.

## 1. Scope and non-goals

### 1.1 Parts and ownership

| Part | Scope | Owns |
| --- | --- | --- |
| `CodeModeSession` | Session | The `codemode` tool declaration, user-facing settings and the Session-side runtime factory. It has no branch business state. Its registered handler is invoked by the core with the current branch execution context; it does not discover a branch by reading Session state. |
| `CodeModeBranch` | branch | One-shot executions associated with the current branch, their cancellation and their correlation with programmatic tool results. It writes no duplicate fact for the outer `step/tool-call`; that fact remains ToolExecutor's contract. |
| `ToolExecutor` extension | branch, mechanism owner | The generic programmatic invocation command/result contract and its normal Permission, Approval, ToolBinding and settlement semantics. This is not Code Mode business state. |
| isolated program host | runtime host | The execution boundary and resource limits for model-supplied code. It receives only the declared tool bridge. |

`CodeModeSession` is an ordinary Session part, not a Capability: it registers a tool and owns no
branch Timeline state. `CodeModeBranch` is a Capability because it consumes branch facts and owns
the state of active programmatic executions. The generic invocation support belongs to the existing
ToolExecutor mechanism owner, not to the Code Mode plugin.

### 1.2 Goals

- Allow one model tool call to run a bounded asynchronous program.
- Let that program invoke tools from the request's fixed ToolBinding snapshot.
- Keep every programmatic invocation under the invoked tool's Permission and Approval policy.
- Record invocation handoff and terminal outcome in the branch Timeline.
- Return one bounded outer `ToolResult`; nested results do not become separate model messages.
- Preserve cancellation, handler pinning, reload settlement and no-replay-of-side-effects semantics.

### 1.3 Non-goals

| Not done here | Why |
| --- | --- |
| Persistent REPL, cells or cross-turn JavaScript state | That is durable execution state and belongs in a separate design or Workflow. |
| Workflow stages, run retention or restore | Workflow owns cross-Session task state; Code Mode is one-turn orchestration. |
| Direct access to `ToolExecutor`, handlers, `ToolHost`, file descriptors or host objects | Those would bypass the existing execution and authorization boundaries. |
| An implicit grant of all nested permissions after approving `codemode` | Each invoked tool remains independently subject to Permission and Approval. |
| A second event bus, lifecycle manager or registry | The existing Timeline, Dispatcher, ToolBinding and ToolExecutor contracts are reused. |
| Automatic context injection of nested results | Only the final outer `ToolResult.content` is model-visible. |
| Automatic retries after cancellation, crash or reload | An external side effect may already have happened; replay is unsafe. |
| Recursive `codemode` calls in the first delivery | They complicate ownership, budgets and settlement without serving the initial use case. |

## 2. Own contract

### 2.1 `codemode` tool

`CodeModeSession` registers one Session tool named `codemode`. Its declaration contains the program
input and optional *lower* budget requests. The plugin and host configuration remain authoritative;
the program cannot raise a limit.

```json
{
  "code": "const files = await tools.list({pattern: \"src/**/*.rs\"}); return files",
  "timeoutMs": 30000,
  "maxCalls": 50
}
```

The exact wire declaration is a binding decision, but the semantic fields are:

| Field | Meaning |
| --- | --- |
| `code` | One asynchronous program. It is rejected when absent, not a string or larger than the configured code limit. |
| `timeoutMs` | Optional requested wall-clock budget, capped by the configured maximum. |
| `maxCalls` | Optional requested nested-call budget, capped by the configured maximum. |

The outer tool has its own declared risk and therefore enters the normal ToolExecutor admission path.
Approving the outer call does **not** approve nested calls.

The outer result is a normal `ToolResult`. Its `content` is the only text Code Mode promises to the
model. A successful result contains the program's final bounded value; an error result names a stable
terminal status and a safe message. `structuredContent` may carry UI/persistence data, but it is not
the model-facing channel for a required handle or status.

### 2.1.1 Impact and public additions

The first delivery changes only the following owners' contracts:

| Owner | Addition | Why it is public |
| --- | --- | --- |
| Session tool binding | `codemode` declaration and result contract | The model must be able to adopt and call the tool. |
| Branch invocation mechanism | A programmatic invocation command/result pair and a branch-scoped caller | Code Mode and the existing tool executor are separate parts and must hand off through a declared contract. |
| Program host | A bounded one-shot execution boundary | Model-supplied code cannot be treated as a trusted in-process function. |

No existing tool declaration, ordinary `step/*` event, context-input route or Workflow contract is
changed. The mechanism addition is generic rather than a Code Mode hook so another automation
capability can use it without making the core depend on this optional plugin.

Terminal statuses are:

```text
completed
failed
cancelled
timed_out
call_limit
concurrency_limit
output_limit
code_limit
reloading
```

The terminal status must distinguish a call explicitly rejected before handoff (`notExecuted`), a
call cancelled before execution (`cancelled`), and a call that may already have taken effect
(`reloading`). An unmatched committed command always uses `reloading` during recovery. The plugin must
not claim rollback.

### 2.2 Runtime bridge

The isolated host receives a narrow bridge, conceptually:

```text
program
  └─ tools.list() -> tool summaries
  └─ tools.describe(name) -> adopted declaration/schema
  └─ tools.call(name, arguments) -> Promise<ToolResult>
```

The runtime has no `process`, environment, filesystem, network, child-process API, arbitrary module
loader, host object, handler reference or Timeline writer. `tools.call` accepts only a name present in
the invocation-scoped declaration snapshot and JSON data validated at the boundary.

The host enforces at least:

```text
max_code_bytes
max_runtime_ms
max_nested_calls
max_parallel_calls
max_single_result_bytes
max_output_bytes
max_memory_bytes
max_recursion_depth = 0 for nested codemode in the first delivery
```

The host is an execution boundary, not a dependency-declaration promise. The implementation and
acceptance of that boundary are tracked by [#1276](https://github.com/arcships/dim/issues/1276).

### 2.3 Invocation contract

The generic mechanism exposes a branch-scoped, language-neutral programmatic invocation contract.
Code Mode is one caller; the contract must not name Code Mode.

A command carries:

```text
invocationId
parentCallId
turnId
requestId
name
arguments
```

`invocationId` is unique within the branch execution. `parentCallId` identifies the outer ordinary
`codemode` tool call. `turnId` and `requestId` identify the pinned request. The command is accepted
only while the parent execution is admitted and before its cancellation boundary.

The terminal result carries:

```text
invocationId
parentCallId
turnId
requestId
status: returned | failed | denied | cancelled | notExecuted | reloading
result: optional ToolResult
message: safe terminal text
```

The result is delivered to the waiting program by `invocationId`; its parent fields also let a new
ToolExecutor generation correlate recovery with the outer `step/tool-call`. It is not a new model tool
result.
The invoked tool's own effects, if any, remain in the returned `ToolResult.effects` contract.

The mechanism resolves `name` and its handler from the request's pinned ToolBinding snapshot. It does
not read a newer ToolRegistry generation and does not expose the handler to the plugin or runtime.
Each invocation then uses the actual invoked declaration's risk for Permission and Approval.

## 3. Events and state

### 3.1 Timeline events

The outer call already uses the core facts:

```text
step/tool-call(codemode)
  -> step/tool-result(codemode)
```

Programmatic calls add a generic pair owned by the ToolExecutor mechanism, not a Code Mode-specific
pair:

```text
tool/invoke
  -> tool/invocation-result
```

`tool/invoke` is the durable handoff command. Code Mode writes it through the current branch
Timeline after validating the program request and before waiting for the result. The append returns
only after the command's batch is committed; publication and ordered Dispatcher delivery then follow
the existing Timeline contract. ToolExecutor consumes it, performs admission and execution, and writes
exactly one terminal `tool/invocation-result`. CodeModeBranch consumes that result and resolves the
corresponding runtime promise.

The events are public because two independently owned parts must understand them. Their contract is
about *programmatic invocation of a pinned tool*, not about Code Mode. They are a generic core event
contract owned by the ToolExecutor mechanism owner, not `CodeMode/*` extension events. The mechanism
must add the `tool` domain to the core's typed event contract and reserve it from extension events.
The context projection does not adopt these execution facts as model messages; this is part of the
core event contract, rather than a flag that Code Mode can use to suppress an otherwise model-facing
fact. Thus the nested facts are durable and recoverable without becoming model context messages.

The events must contain enough correlation to settle after cancellation or reload without consulting
runtime heap state. In particular, the result must identify the invocation and parent request, and a
terminal result must distinguish `notExecuted` from `cancelled`/`reloading` where the call may have
already taken effect.

These events are not ordinary `step/tool-call` facts. `step/tool-call` represents a batch emitted by
the model and is consumed by the normal turn/tool batch state machine. `tool/invoke` represents a
programmatic child call of an already-running outer tool. Reusing `step/tool-call` would make Loop and
ToolExecutor interpret a child as a second model batch and would corrupt batch ownership and context
semantics.

### 3.2 State and rebuild

`CodeModeBranch` keeps only runtime handles and waiters in memory. The business state needed to settle
an invocation is the ordered Timeline state:

| State | Owner | Rebuild |
| --- | --- | --- |
| invocation handed off | Timeline / ToolExecutor | From `tool/invoke` without a terminal result |
| invocation terminal status | ToolExecutor | From `tool/invocation-result` |
| active program and its promises | CodeModeBranch / isolated host | Runtime-only; cancelled and settled on close or reload, never replayed |
| outer turn result | ToolExecutor | From `step/tool-result`; on recovery, ToolExecutor derives a reload/cancel warning from the outer call's correlated child facts |

On reload, an invocation with no terminal result is settled as `reloading` by ToolExecutor. The
committed `tool/invoke` is the handoff boundary: a new generation must not classify an unmatched
command as `notExecuted`, because it cannot prove that the handler never started. The terminal result
therefore warns that the effect may have taken place, and the invocation is never executed again.
`notExecuted` is reserved for an invocation that ToolExecutor explicitly rejects and settles after
admission; it is not a recovery inference for an unmatched command. The same handoff rule is stable
for all callers. If cancellation occurs before dispatch, ToolExecutor may settle the committed
invocation as `cancelled`, with the same no-replay rule.

The outer `codemode` result has the same recovery protection. ToolExecutor remains the sole writer of
`step/tool-result`; when rebuilding an open outer call, it correlates `parentCallId` with any committed
child `tool/invoke` facts. If a child exists without a terminal result, or has a terminal status (`returned`, `failed`,
`cancelled` after admission or `reloading`) that permits an effect to have occurred, ToolExecutor
writes the outer reload/cancel result with an explicit effect-may-have-happened warning instead of
the generic `Tool was not executed` text. When
CodeModeBranch is still alive, its branch-bound invocation port returns that same terminal result to
the outer handler; it does not append a competing outer result.

The first delivery does not write `codemode/started` or `codemode/finished`: the outer `step` facts
already record the tool call, and the nested generic pair records every external child operation.
Adding a duplicate Code Mode state domain would create facts with no later decision depending on them.
If a later UI or recovery requirement needs a durable Code Mode run projection, that domain must be
added as a separate design with an explicit consumer.

### 3.3 Approval and Permission

There are two authorization decisions:

```text
outer codemode call
  -> Permission / Approval for codemode's declared risk

nested tool/invoke(name)
  -> Permission / Approval for name's declared risk
```

Approval facts remain the existing `approval/request`, `approval/respond` and cancellation contract.
A nested approval must carry the generic invocation's parent correlation so the client can explain
what is waiting, but it must not grant authority to sibling calls or to future invocations. Permission
must see the actual pinned tool call and its risk, not the outer Code Mode declaration.

## 4. Dependencies

| Dependency | Scope | Purpose |
| --- | --- | --- |
| `toolRegistry` | Session | Register `codemode` and obtain the adopted tool declaration source. Code Mode does not obtain handlers. |
| invocation-scoped `ToolBinding` | branch | Supply the fixed declarations and handlers for the outer request. |
| generic programmatic invocation service | branch | Submit `tool/invoke`, receive terminal results and retain parent correlation. This is the requirement tracked by [#1276](https://github.com/arcships/dim/issues/1276). |
| `Timeline` | branch | Commit invocation commands and terminal results through the branch's existing timeline service. |
| `Dispatcher` | branch | Consume terminal invocation results in sequence order. |
| `BranchLifecycle` | branch | Delay background execution until publication and stop admission at closing. |
| `Approval` and `Permission` | branch | Reuse the existing nested authorization decision for each actual tool. |
| isolated program host | runtime | Execute one program under hard limits; the requirement is [#1276](https://github.com/arcships/dim/issues/1276). |

The plugin does not depend on `ToolHost`, a handler registry, a second event bus, or direct Session
facade access. A TypeScript binding must expose the generic service as a declared branch dependency;
undeclared access is not an optional lookup.

## 5. Requirements raised to mechanism owners

### R1. Generic Timeline-backed programmatic tool invocation

| Field | Content |
| --- | --- |
| Problem | Code Mode needs to hand a child tool call to the current branch's ToolExecutor while the outer `codemode` call is running. Rule §4.7 requires public event/service interaction, and ToolExecutor's existing `step/tool-call` contract represents a model batch, not a child call. The Rust execution context already carries `branch_id`, but the current TypeScript Session tool callback receives only call data and cancellation; it has no branch-bound invocation port for safely submitting the command to that branch. |
| Existing mechanisms considered | Calling a registered handler or `ToolHost` bypasses ToolBinding pinning, Permission, Approval, cancellation, settlement and reload. Writing a nested `step/tool-call` makes Loop and ToolExecutor treat the child as a second model batch. The current TypeScript binding exposes no safe executor entry point or branch-bound invocation service. |
| Generic capability | A branch-scoped, language-neutral programmatic invocation contract with `tool/invoke` and `tool/invocation-result`, request-pinned handler resolution, parent correlation, actual-tool risk admission, normalized results and no handler exposure. The outer tool callback must receive a per-call, branch-bound data port (or equivalent context) for this contract; it must not receive a raw Timeline, ToolExecutor or handler. The capability must be usable by automation other than Code Mode. |
| Sequence, batch, interruption and reload | `tool/invoke` is committed as the handoff boundary and is consumed in branch sequence order. Multiple child calls may belong to one outer call, subject to a caller budget, without creating another model batch. Cancellation stops new admission and settles committed work. Reload settles every unmatched command as `reloading` with an effect-may-have-happened warning; it never infers `notExecuted` and never replays an effect. ToolExecutor also derives the outer `codemode` reload/cancel result from correlated child facts. Nested results do not enter context automatically. |
| Owner and issue | Dim runtime / ToolExecutor mechanism owner; [#1276](https://github.com/arcships/dim/issues/1276). |

### R2. Isolated bounded program host

| Field | Content |
| --- | --- |
| Problem | A model-supplied program cannot be safely run with direct `eval` or a trusted in-process plugin. Dependency declarations do not sandbox Node or host resources. |
| Existing mechanisms considered | The TypeScript plugin process, ordinary module loading and direct host calls expose too much authority. A timeout alone does not prevent filesystem, process, network or secret access and does not enforce memory/output limits. |
| Generic capability | A one-shot isolated host with only a serializable tool bridge and hard code, wall-clock, memory, output, invocation and concurrency limits. It must return a terminal status and terminate on cancellation. |
| Sequence, batch, interruption and reload | Host termination cancels the program but does not undo a child already handed to ToolExecutor. The host joins or explicitly settles its worker before the outer tool result. Reload produces a terminal host outcome and leaves Timeline settlement to the invocation owner. |
| Owner and issue | Runtime/host mechanism owner; [#1276](https://github.com/arcships/dim/issues/1276). |

## 6. Rule check

The governing rules are [principles §1](../../architecture/rust-agent/principles.md#1-插件部件与-capability-有界),
[§4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) and [ADR 002 D1–D5](../../adr/002-plugin-scope.md#decisions).

| Rule | How this design complies |
| --- | --- |
| §1 Capability boundary | `CodeModeBranch` is the only Code Mode Capability. `CodeModeSession` only registers a tool. Generic nested invocation and the isolated host are mechanism-owner contracts, not hidden Code Mode state. |
| §4.1 split by state owner | Session owns the tool declaration/settings; Branch owns active execution and branch facts; ToolExecutor owns generic invocation admission and results; the isolated host owns only runtime resources. |
| §4.2 command and handoff | `tool/invoke` is the command/handoff. Before it commits, Code Mode can reject the call. After handoff, only ToolExecutor's cancellation/settlement contract applies; dropping a Promise is not rollback. |
| §4.3 handlers register work and return | Timeline handlers register execution/waiter tasks and return. Runtime/tool waits are owned by CodeModeBranch or ToolExecutor; resources are released before terminal results are written. |
| §4.4 core mechanism has no business meaning | The requested seam is `programmatic tool invocation`, not `CodeModeExecutor`. The event names and fields describe a generic child tool call and can serve other automation. |
| §4.5 atomic batches and ordered dispatch | Each command and terminal result is committed atomically and dispatched by sequence. Child calls do not create a second model batch; result delivery follows invocation correlation. |
| §4.6 context boundary | Nested invocation facts are execution facts and are not adopted as model messages. Only the adopted outer `step/tool-result` is model-facing. A value the model must use is included in the outer `ToolResult.content`. |
| §4.7 public interaction | Parts communicate through the declared branch service and public generic invocation facts. No handler, ToolExecutor object or private plugin state crosses the boundary. |
| §4.8 infrastructure requirements | R1 and R2 identify the existing contracts considered, the generic mechanisms needed, sequence/batch/interruption/reload effects and their owners. The design does not add a Code Mode-specific core hook. |
| §4.9 facts in Timeline | Each external child operation has a durable command/result pair because recovery, approval correlation and no-replay settlement depend on it. Pure program computation and diagnostics stay out of Timeline. No duplicate `codemode/*` status facts are added in the first delivery. |
| ADR 002 D1 | Parts follow state and lifecycle owners: Session tool, Branch execution, and process-free one-shot runtime. |
| ADR 002 D2 | No wiring plugin, registry adapter, second event bus or new lifecycle manager is introduced. |
| ADR 002 D3 | Dependencies point from Code Mode to existing core contracts and the generic mechanism; core does not depend on the optional plugin. |
| ADR 002 D4 | Limits and user settings are business configuration read at use time. Enabling/reloading the plugin is structural configuration; changing a limit does not require a new assembly mechanism. |
| ADR 002 D5 | The invocation and runtime contracts are language-neutral data and error shapes; Rust and TypeScript spellings belong in the binding appendix. |

Ablation: the design intentionally removes a persistent REPL, a Code Mode-specific core event domain,
a second tool registry, direct handler access, automatic nested context messages, implicit nested
authorization and automatic retry. The outer `step` facts plus the generic invocation command/result
pair are sufficient for first-delivery recovery and authorization; adding another durable execution
domain would duplicate state without a consumer.

## 7. Binding appendix

The following is semantic pseudocode, not a committed Rust or TypeScript spelling.

### 7.1 Session registration

```text
CodeModeSession.apply(ctx):
  toolRegistry.register(
    declaration = {
      name: "codemode",
      source: "extension",
      risk: configured_outer_risk,
      inputSchema: { code, timeoutMs?, maxCalls? }
    },
    execute = (call, cancelled) => currentBranchCodeMode.run(call.arguments, cancelled)
  )
```

The actual binding must use the existing Session tool registration contract and return the normal
`{content, isError?}` result shape. It must validate external input once at the boundary.

### 7.2 Branch invocation service

```text
programmaticToolInvoker.invoke({
  parentCallId,
  turnId,
  requestId,
  name,
  arguments,
  cancelled
}) -> {
  invocationId,
  status,
  result?,
  message?
}
```

The service is branch-scoped and declared by the consuming part. It accepts serializable data only.
The implementation owns the route from `tool/invoke` to the pinned ToolBinding and from
`tool/invocation-result` back to the caller.

### 7.3 Runtime outcome

```text
run(program, limits, tools) ->
  { status: "completed", value }
  | { status: "failed" | "cancelled" | "timed_out" | "call_limit" |
      "concurrency_limit" | "output_limit" | "code_limit" | "reloading",
      message }
```

The host never returns a handler, host capability or unbounded result. A runtime outcome is converted
once to the outer ToolResult, whose `content` remains the only model-facing text.
