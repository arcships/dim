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
| `B:Dispatcher` | Replay whether a turn is open, then consume arrivals and turn boundaries. |

### Events and lifetime

Consumes `input/message`, `turn/start`, `turn/end`; produces no events directly.
Tagged background notices and steer inputs can end a wait; the input opening a
turn does not. Turn end clears pending arrivals. It registers with
`Dispatcher::follow`. Reload rebuilds only open-turn status, not old arrival
notifications. Wait futures are caller-owned and use caller cancellation; the
plugin does not spawn tasks. Only one wait can run per branch.

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`,
`wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait`.

<!-- generated:plugin WaitBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher` |
| 提供 | `B:Wait` |
| 被注入于 | — |
<!-- /generated -->
