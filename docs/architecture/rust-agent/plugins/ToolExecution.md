# ToolExecution

The ToolExecution plugin registers the Session [ToolMethods](ToolMethods.md) read part and two branch parts: ToolBinding, which adopts the Session's tools into the branch's model configuration, and ToolExecutor, which runs committed tool batches.

## ToolBinding

Owner: [tool_binding.rs](../../../../crates/dim-agent-capabilities/src/tool_binding.rs).
Adopts a Session tool set into branch model configuration and pins handlers to
requests so later registry changes cannot silently replace executing tools.
ToolBinding carries the declarations it adopts and owns no wording. It fixes
each request's declarations exactly as their owners wrote them. How a
declaration splits into protocol and policy is ToolRegistry's
[Declarations](ToolRegistry.md#declarations) rule.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` | Current tools and their source-state watch (`sources_state()`). |
| `B:Prompt` | Adopt tool configuration and await initial prompt completion. |
| `B:Timeline` | Reconcile committed configuration/request pins. |
| `B:BranchLifecycle` | Gate background refresh on activation/closing. |
| `B:Dispatcher` | Handle branch opening. |
| `B:BranchOpening` | Register initial tool-adoption completion receipt. |

### Events and lifetime

Consumes `branch/start` and `branch/resume`. Configuration facts are published
through Prompt rather than a separate tool-event route. Opening work and a
adoption task are tracked and joined on cleanup. It takes the registry's
source-state watch before its initial registry read. A forwarding task only wakes
its own adoption task; an update before activation remains pending until the
existing BranchLifecycle opens. The forwarding task ends with its context or the
registry. Reload reconciles the
already adopted configuration. Initialization reads the registry even without
initial model configuration, fixing the tools registered then before the first
input. Every registry read, request preparation, `bind_current` and the adoption task's included,
takes in what is registered at that moment, so a tool registered later is
adopted for the next request. With a model configuration, opening reconciles its tools through Prompt before
the existing BranchOpening receipt completes. Failed adoption retains pins and the prefix;
a later change retries. Pins are runtime handles, not persistent executable
identities; missing tools cannot be recovered by replaying their old definitions.

For a request without adopted configuration, `prepare` retains one registry
snapshot while the caller selects the declaration. Binding validates that
selection against the retained handlers, even if a provider retires or registers
a replacement during preparation. A retired handler follows its own stopping
contract; it is not replaced by a current handler of the same name. Failed
preparation drops its source references without registering a request.

For a continuation request ([Loop](Loop.md#a-failed-model-step-and-its-disposer)),
request preparation declares no tools and ToolBinding fixes an empty snapshot
for that request. The choice belongs to the Loop's request preparation and has
no separate flag: the committed `context/prepare` already records the declared
tools.

### Rule check

| Rule | Application |
| --- | --- |
| §4 rule 1 | Prompt still owns configuration facts; notifications only request a reread |
| §4 rule 2 | Accepted request snapshots retain their selected handlers |
| §4 rule 3 | No before callback |
| §4 rule 4 | No core or plugin-specific assembly change |
| §4 rule 5 | Prompt retains its existing atomic configuration commit |
| §4 rule 6 | Tool declarations reach the model only through adopted configuration |
| §4 rule 7 | Uses the registry's public snapshot and source-state watch |
| §4 rule 8 | The registry's existing watch, BranchLifecycle and adoption task suffice |
| §4 rule 9 | Change notifications stay outside the Timeline |
| §1, ADR 002 D1 | Adoption and its pending wake belong to this branch capability |
| ADR 002 D2 | No wiring plugin, new bus or shared notification channel |
| ADR 002 D3 | Existing declared dependencies remain unchanged; registered tools are registry sources (#241), and a configuration without a tool part adopts none of their changes |
| ADR 002 D4 | No configuration or reload policy change |
| ADR 002 D5 | Source changes are read from one Session's registry watch; no wire protocol change |

### Requirements raised to mechanism owners

None. The forwarding task wakes the capability's existing task through a private
`Notify`; it does not hand that primitive to another plugin. The permit retains
updates before branch activation and coalesces subsequent rereads. No additional
task, mutex, lifecycle phase or protocol field is needed.

### Tests

[tool_binding.rs](../../../../tests/integration/tests/tool_binding.rs):
`a_call_runs_the_version_its_request_declared_and_old_versions_are_released`,
`a_restored_branch_adopts_the_new_versions_before_its_first_request`,
`contributors_are_current_for_the_first_request_without_initial_configuration`,
`a_preparing_request_keeps_its_contributor_generation`,
`custom_preparation_keeps_none_empty_and_filtered_declarations`,
`tool_updates_wait_for_branch_activation_and_do_not_run_after_failed_assembly`.

<!-- generated:plugin ToolBinding — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:BranchOpening`、`B:Dispatcher`、`B:Prompt`、`B:Timeline`、`S:ToolRegistry` |
| 提供 | `B:ToolBinding` |
| 被注入于 | `Loop`、`ToolExecutor` |
<!-- /generated -->

## ToolExecutor

Owner: [tool_executor.rs](../../../../crates/dim-agent-capabilities/src/tool_executor.rs).
Executes a committed tool batch against pinned handlers after approval, isolates
individual call failures and publishes correlated results.

Before admitting a `write_filesystem` call, the pinned handler's `resolve_write`
may resolve its target using metadata only. Approval and execution receive that
same resolved call; the committed model call is unchanged. A resolver cannot
mutate files or start processes before approval. Permission's [workspace write
contract](Permission.md#write-targets-without-the-facts) decides whether the target asks.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Approval` | Admit each call under permission/approval policy. |
| `B:ToolBinding` | Resolve the request's pinned tools. |
| `B:TimelineIdentity` | Construct the calling branch execution context; Session and branch ids of the `tool.execute` span. |
| `B:Dispatcher` | Replay batches, receive batches/interrupts and report faults. |
| `B:Timeline` | Commit progress/results. |

### Events and lifetime

Consumes `step/tool-call`, `stop/interrupt`, `turn/start`, `turn/end`; produces
`step/tool-start`, `step/tool-completed` when progress is enabled and
`step/tool-result`. It records each returned result unchanged, so the effect
declarations on it travel on that fact
([session boundary A.6](../../../design/contracts/session-boundary.md#a6-tool-effects));
a call it settles without a tool result carries none. Calls in a batch settle
before the correlated batch result.
One top-level batch runs at once. A child batch carries `parent_call_id`; the
top-level consumer ignores child batches, and the nested gate writes their
`step/tool-call`/`step/tool-result` and joins them before the outer result. Child
batches may run in parallel and may themselves carry grandchildren. Interrupts
cancel current work, including child calls, and suppress late host invocation.
Cleanup closes admission, cancels, joins tracked work and aggregates failures.
The child gate bounds its join at five seconds; a handler that ignores its
cancellation token past the grace is outside the contract and its child fact may
settle after the outer result. Child events are execution facts:
`context.render` and `context.state` are false for an event with
`parent_call_id`, so child results never enter model context. Reload settles open
child batches first as errors, then settles an open outer batch with the
confirmed effects of terminal child results; child results keep their effects. Reload-settled results make Loop fail the turn. Recovery selects
batch identities without a matching `call_event_id` result, reads their turn,
request, call and parent identities, and does not read tool arguments.
For an unfinished outer batch, one query selects child result effects for its
known parent call IDs. Each parent receives only its own children's effects;
arguments and result text are not read. During cancellation this query omits
parents whose returned result already contains effects, and is skipped when
none need it. State is not cached beyond settlement. Interrupt
state comes from the latest interrupt or turn boundary at the same fixed read
boundary. Cancellation is not rollback of a tool's completed side effect.

A request prepared with no tools never runs a call: the executor's snapshot
refusal ([tool_binding.rs](../../../../crates/dim-agent-capabilities/src/tool_binding.rs))
means no call from that request is ever dispatched. ModelProvider states the
stream refusal and the request's settlement
([ModelProvider](ModelProvider.md#failures)).

### Model-facing texts

The results of a call that did not complete, and of a call whose result was not
delivered, are protocol
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):
`Tool was not executed: <reason>`, whose reasons include
`progress admission failed` and `capability reloading`, and `Tool cancelled
before execution` say the work did not run; `Tool execution cancelled:
capability reloading` says the call may have run, possibly to the end, and that
its result was replaced because the capability reloaded. An open outer batch
with child results says `Tool did not finish before Capability reload; an effect
may have happened` and carries the confirmed child effects. The model checks the
effect before it retries, and reports the work neither as done nor as never run.

### Tests

[interrupt_flow.rs](../../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs):
`a_tool_call_already_committed_when_the_interrupt_arrives_is_not_executed`;
[review_flows.rs](../../../../crates/dim-agent-capabilities/tests/review_flows.rs):
`one_tool_panic_does_not_abandon_other_native_cleanup_or_reorder_actual_results`.

<!-- generated:plugin ToolExecutor — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Approval`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding` |
| 提供 | `B:ToolExecutor` |
| 被注入于 | — |
<!-- /generated -->

ToolBinding reads the latest `context/configured` settings at a fixed chain head
when adopting tools; it does not project the conversation to find that configuration.
