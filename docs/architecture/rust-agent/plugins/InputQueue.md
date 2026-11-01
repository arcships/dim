# InputQueue

The InputQueue plugin registers InputMethods in every Session and InputQueue in every branch.

## InputMethods

Owner: [input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs).
Registers the namespace `session.input` with the Session's `sessionMethods`:
`session.input.withdraw` for every branch of its Session, served through that
branch's InputQueue. It also provides the Session table `submitChecks`
([`SubmitChecks`](../../../../crates/dim-agent-capabilities/src/input_queue.rs)):
Session parts register submit checks with it from their own `apply` and the
registration is withdrawn when that part unloads. A `session.input.withdraw`
with `steer` calls each check registered now, in name order, before its batch;
any block fails the call with `input_blocked` and writes nothing, else the
checks' contexts become text blocks after the steer's own content. This is
Hooks A.6.2's generic capability; InputQueue gives the checks no meaning, and
with none registered the withdrawal writes unchecked.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Find the branch named by a call and read its InputQueue. |
| `S:SessionMethods` | Register `session.input` from `apply`, until this part unloads. |

### Events and lifetime

Writes no events itself; InputQueue commits `input/withdraw` and its result.
The registration follows the shared Session method admission: an unload
interrupts a call, and a caller's disconnect cancels nothing. The submit
checks are consulted inside that call; only its own interruption cancels
them. While the part reloads, the withdrawn namespace answers
`service_reloading`.

### Tests

`input_queue.rs`'s unit tests pin the table's own rules
(`checks_run_in_name_order_and_the_first_block_answers`,
`proceeding_checks_add_their_blocks_in_name_order`,
`a_check_that_fails_counts_as_proceed`, `an_interrupted_call_runs_no_check`,
`an_interrupted_call_stops_a_running_check`,
`a_second_check_under_a_name_fails_until_its_part_unloads`,
`an_unloaded_part_withdraws_its_checks`);
[session_methods.rs](../../../../crates/dim-app-server/tests/transport_flow/session_methods.rs)
covers `session.input.withdraw` through Rpc;
[hooks_flow.rs](../../../../crates/dim-agent-capabilities/tests/hooks_flow.rs)
covers a check blocking a steer (`a_blocked_steer_writes_nothing_and_the_old_input_still_applies`);
[submit_checks.rs](../../../../crates/dim-app-server/tests/transport_flow/submit_checks.rs)
covers the table and the Rpc entries.

<!-- generated:plugin InputMethods — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionMethods`、`S:TreeCommands` |
| 提供 | `S:SubmitChecks` |
| 被注入于 | `HooksSession` |
<!-- /generated -->

## InputQueue

Owner: [input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs).
Decides when queued, append and steer inputs become applicable and publishes the
observable input queue.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Replay the queue, then consume input/turn/interrupt transitions. |
| `B:Timeline` | Commit application decisions. |

### Events and lifetime

Consumes `input/message`, `input/consumed`, `turn/start`, `turn/end`,
`stop/interrupt`; produces `input/applied`. It does not run a model itself.
It registers with `Dispatcher::follow`; the replay writes owed application
decisions, without duplicating already committed ones, before any live event. No background tasks or
external resources. Reload reconstructs state from local history. An idle append
is pending until a driving input; an interrupt is not permission to start a new
turn from previously unapplied input.

### Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs):
`steer_and_append_enter_the_current_turn_at_tool_result_while_queue_waits`;
[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs):
`restoring_pending_inputs_runs_each_queued_input_in_its_own_turn`.

<!-- generated:plugin InputQueue — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:InputQueue`、`B:StateCell<InputSnapshot>` |
| 被注入于 | — |
<!-- /generated -->
