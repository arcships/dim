# InputQueue

The InputQueue plugin registers InputMethods in every Session and InputQueue in every branch.

## InputMethods

Owner: [input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs).
Registers the namespace `session.input` with the Session's `sessionMethods`:
`session.input.pending` ([contract](../../../design/input-state-read.md)) and
`session.input.withdraw` for every branch of its Session, served through that
branch's InputQueue. It also provides the Session table `submitChecks`
([`SubmitChecks`](../../../../crates/dim-agent-capabilities/src/input_queue.rs)):
Session parts register submit checks with it from their own `apply` and the
registration is withdrawn when that part unloads. A `session.input.withdraw`
with `steer` calls each check registered now, in name order, before its batch;
any block fails the call with `input_blocked` and writes nothing, else each
context that is one runtime marker element becomes a text block after the
steer's own content, and one that is not is dropped with a diagnostic. This is
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
| 被注入于 | `HooksSession`、`Skills` |
<!-- /generated -->

## InputQueue

Owner: [input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs).
Decides when queued, append and steer inputs become applicable and publishes the
observable input queue.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Hand off restored queue state to live input/turn/interrupt transitions. |
| `B:Timeline` | Commit application decisions. |

### Events and lifetime

Consumes input, withdrawal, turn and interrupt facts; produces only the original
application/deletion decisions. It writes no queue recovery or node events.

Recovery fixes a published cutoff, selects ordinary and handover current members,
then reads last applied/withdraw controls for each page of at most 128 identities.
Two last turn boundaries and an interrupt only when needed determine turn/starting.
It never reads input bodies to recover entries; old missing input modes use
one identity-driven user-presence query for all missing modes in a page, returning only IDs. Applied mode defaults agree with Loop.
Current entries, queued count, turn, starting and release bits stay in memory.
Only necessary applied/deleted output waits for its commit before publishing the
observable queue; empty or unchanged transitions do not publish a new count.
Public pending semantics still exclude handovers. Old derived recovery records
are ignored; original facts and native member indexes remain authoritative.

### Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs):
`steer_and_append_enter_the_current_turn_at_tool_result_while_queue_waits`,
`a_handover_input_is_held_without_being_applied_or_counted`,
`a_handover_input_is_deleted_at_its_turn_end_and_never_reaches_the_model`,
`a_handover_input_for_no_open_turn_is_deleted_on_arrival`,
`a_consumed_handover_input_is_not_deleted_at_turn_end`,
`a_held_handover_input_does_not_steal_the_next_queued_input`,
`a_handover_input_written_while_idle_starts_no_turn`,
`a_withdrawn_handover_input_is_deleted_once`;
[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs):
`restoring_pending_inputs_runs_each_queued_input_in_its_own_turn`,
`a_restored_queue_writes_one_owed_deletion_for_a_handover_input`.

<!-- generated:plugin InputQueue — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:InputQueue`、`B:StateCell<InputSnapshot>` |
| 被注入于 | — |
<!-- /generated -->
