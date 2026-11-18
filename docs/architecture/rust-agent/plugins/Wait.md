# Wait

The Wait plugin registers Wait in every Session and WaitBranch in every branch.

## Wait

Owner: [wait.rs](../../../../crates/dim-agent-capabilities/src/wait.rs).
Registers the Session wait tool, resolving the caller's branch-local Wait
service to await a matching task notice, a steer from the user or a line, a
timeout or an interruption.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Resolve the calling branch and its Wait service per call. |
| `S:ToolRegistry` | Register `wait`, until this part unloads. |

### Events and lifetime

No direct event production/subscription; the outcome is a tool result.
WaitBranch records arrivals. Futures use caller cancellation and release the
single-wait slot on exit/drop. The timeout is an integer from 1 through 300,000 ms,
inclusive, with default 300,000 ms; it bounds this wait, not task execution.
Both the tool schema and runtime enforce that range. WaitBranch generation termination ends
an accepted tool wait through the reload-settlement path. Generation changes
can make per-call branch lookup fail as reloading; this contribution itself has
no tracker or disposer. It is not a process join primitive: cancelled background
tasks emit no wake and old arrivals are not replayed after WaitBranch reload.

### Model-facing texts

The `wait` declaration is protocol as well
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):
its schema, and the description's two sentences — "Wait until a background task
(like exec or agent) finishes, the user or a line of this session sends you a
steer, or the timeout elapses." states when a wait ends, which the results
repeat, and "Pass a taskId to wait for one specific task, or omit it to wake on
any." states the call shape. Its result texts are protocol too.
`wait ended: notification (<task ids>)` names the tasks that finished, and those
ids are the task handles the model passes back to `wait` or `exec_stop`; `read`
takes the output path from the `exec` result's `Output: <path>`. `wait ended:
notification (<task ids>) was already delivered above.` answers a call naming a
task whose notice the model already read in this turn, so that call does not wait
for a notice which will not come again. `wait ended: steer`, `wait ended:
interrupted` and `wait ended: timeout` state the other ways a wait ends. The
refusals — `wait taskId must be a string.`,
`wait timeoutMs must be less than or equal to <limit>.`,
`wait timeoutMs must be a positive integer.` and
`wait is already running in this session.` — are protocol too: each names the
input it refused and how the input is wrong.

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`,
`wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait`,
`a_steer_the_model_read_does_not_end_a_later_wait`,
`a_background_notice_the_model_read_does_not_end_a_later_wait`,
`wait_reports_a_task_whose_notice_the_model_already_read`,
`wait_reload_settles_the_tool_call_without_a_user_stop`,
`wait_service_reload_ends_native_and_face_calls_and_refuses_old_handles`.

<!-- generated:plugin Wait — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:ToolRegistry`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## WaitBranch

Owner: [wait.rs](../../../../crates/dim-agent-capabilities/src/wait.rs).
Provides branch-local wait state recording eligible arrivals during an open turn.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Consume arrivals and turn boundaries in order. |
| `B:Timeline` | Read the last `turn/start` or `turn/end` and the unpublished tail, so the rebuild knows whether a turn is open. |

### Data face

`wait` is this part's service by name
([service faces §3.4](../../../design/service-faces.md#34-对象句柄拆成操作加-id)):

```text
wait.wait(taskId: string | null, timeoutMs: number, cancelled: () => void | Promise<void>)
  -> {reason: "notification", taskIds, waitedMs}
   | {reason: "user_steer", waitedMs} | {reason: "already_notified", taskIds, waitedMs}
   | {reason: "timeout", waitedMs} | {reason: "interrupted", waitedMs}
  -> error {code: "AlreadyWaiting" | "Reloading", message}
```

It is the same wait the tool call makes. `cancelled` is the call's own
cancellation and ends it as `interrupted`, releasing the single-wait slot.
Dropping the native Rust future also releases the slot. Bridge callers must
signal `cancelled` before abandoning the reply; dropping the bridge reply alone
does not cancel the wait. The Rust error binding is `WaitCallError`, with `AlreadyWaiting` and
`Reloading` variants; the existing `AlreadyWaiting` wire code is preserved. Only the tool caps
`timeoutMs` at 300,000; the service uses the supplied timeout.

### Events and lifetime

Consumes `input/message`, `input/applied`, `input/consumed`, `input/deleted`,
`turn/start`, `turn/end`; produces no events directly. A steer or a tagged
notice counts from its `input/applied` and stops counting once the model reads it
(`input/consumed`) or it is deleted, so only an input the model has not read yet
can end a wait; the input opening a turn is released before `turn/start`, so it
never ends a wait. Turn end clears pending arrivals and the tasks whose notice
the model read in that turn. It registers with `Dispatcher::consume` and rebuilds
by selecting only the kind of the last `turn/start` or `turn/end` at the
committed head, including its unpublished tail.
Reload rebuilds only open-turn status, not old arrival notifications. Wait futures are caller-owned,
but remain bound to their service generation. Unload closes
admission, ends active calls with `Reloading` (`capability reloading`) and waits for those calls
to release their slots. Retained native service handles refuse new calls with the same error;
a retired data-face handle is rejected by the existing framework service-resolution boundary as `unavailable`.
The tool translates that error through the existing [reload settlement protocol](../single-tree.md#能力重载协议):
ToolExecutor writes the marked error result and Loop fails the turn without continuing it.
Ordinary caller cancellation still returns `interrupted`. The plugin tracks accepted calls without
spawning tasks. Only one wait can run per branch; old-generation calls end before replacement
completes.

### Reload argument

Admission and unload synchronize through the same state lock. Admission checks the generation
and registers the accepted call before releasing that lock. Unload cancels the generation and
closes the call tracker under the lock, then drains it. Consequently a call either belongs to
the drain or sees a terminated generation; none can be admitted after the drain has passed it.
Each call releases its waiting slot before releasing its tracking token, including native future
drop. Reload wakes blocked waits independently of caller cancellation. Progress assumes accepted
futures are polled or dropped by their callers; the host lifecycle deadline still applies.

The new generation reconstructs open-turn status through the existing precise query and starts
with no old notifications. There is no migration of pending calls. ToolExecutor retains ownership
of tool-result facts; translating `Reloading` to its existing host cancellation reason lets its
normal settlement path end the failed turn. Accepted native and data-face calls report the same
business error; retired face handles remain subject to framework availability errors. No event or persistence format is added.

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`,
`wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait`,
`a_steer_the_model_read_does_not_end_a_later_wait`,
`a_background_notice_the_model_read_does_not_end_a_later_wait`,
`wait_reports_a_task_whose_notice_the_model_already_read`,
`wait_reload_settles_the_tool_call_without_a_user_stop`,
`wait_service_reload_ends_native_and_face_calls_and_refuses_old_handles`.

<!-- generated:plugin WaitBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:Wait`、`B:dyn HostDispatch#wait` |
| 被注入于 | — |
<!-- /generated -->
