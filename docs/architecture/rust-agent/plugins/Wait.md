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
Both the tool schema and runtime enforce that range. Generation changes
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
steer`, `wait ended: interrupted` and `wait ended: timeout` state the other ways
a wait ends. The refusals — `wait taskId must be a string.`,
`wait timeoutMs must be less than or equal to <limit>.`,
`wait timeoutMs must be a positive integer.` and
`wait is already running in this session.` — are protocol too: each names the
input it refused and how the input is wrong.

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`,
`wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait`.

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
([service faces §3.4](../../../design/service-faces.md#34-对象句柄拆成操作加id)):

```text
wait.wait(taskId: string | null, timeoutMs: number, cancelled: () => void | Promise<void>)
  -> {reason: "notification", taskIds, waitedMs}
   | {reason: "user_steer", waitedMs} | {reason: "timeout", waitedMs} | {reason: "interrupted", waitedMs}
  -> error {code: "AlreadyWaiting", message}
```

It is the same wait the tool call makes. `cancelled` is the call's own
cancellation and ends it as `interrupted`, releasing the single-wait slot.
Dropping the native Rust future also releases the slot. Bridge callers must
signal `cancelled` before abandoning the reply; dropping the bridge reply alone
does not cancel the wait. Only the tool caps `timeoutMs` at 300,000; the service
uses the supplied timeout.

### Events and lifetime

Consumes `input/message`, `turn/start`, `turn/end`; produces no events directly.
Tagged background notices and steer inputs can end a wait; the input opening a
turn does not. Turn end clears pending arrivals. It registers with
`Dispatcher::consume` and rebuilds by selecting only the kind of the last
`turn/start` or `turn/end` at the committed head, including its unpublished tail.
Reload rebuilds only open-turn status, not old
arrival notifications. Wait futures are caller-owned and use caller cancellation; the
plugin does not spawn tasks. Only one wait can run per branch.

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`,
`wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait`.

<!-- generated:plugin WaitBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:Wait`、`B:dyn HostDispatch#wait` |
| 被注入于 | — |
<!-- /generated -->
