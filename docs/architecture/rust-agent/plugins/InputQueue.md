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
with `steer` validates the operator's content and calls each check registered
now, in name order, before admitting its command;
any block fails the call with `input_blocked` and writes nothing, else each
context that is one runtime marker element becomes a text block after the
steer's own content, and one that is not is dropped with a diagnostic. This is
Hooks A.6.2's generic capability; InputQueue gives the checks no meaning, and
with none registered the validated candidate has no submit-check additions.
The checked candidate is passed inside the withdrawal command, not submitted
as an independent input. Passing checks does not reserve the held input;
InputQueue decides conditional acceptance in sequence order
([Session access A.4](../../../design/contracts/session-access.md#a4-withdrawal)).

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
| `B:Timeline` | Commit withdrawal commands and atomic application or withdrawal results. |

### Events and lifetime

Consumes `input/message`, `input/consumed`, `input/withdraw`, `input/applied`,
`input/deleted`, `turn/start`, `turn/end` and `stop/interrupt`, including result
facts needed by recovery. Its withdrawal service writes `input/withdraw`;
its decisions produce `input/applied`, `input/deleted` and, only for an accepted
queued-input Steer, the replacement `input/message`. It does not run a model itself.
It registers with `Dispatcher::consume`; recovery reads ordered identity, input
mode, handover identity and turn identity fields in pages at one committed boundary. User-content shape
distinguishes the default queue mode from an internal append; content text is
not otherwise needed for queue decisions. Recovery also reads the optional full
checked `steer` candidate from withdrawal commands and retains it for an owed
conditional result; it never reruns external submission checks. Recovery retains
queue entries and owed decisions, removes each owed decision when its written
result is read, and writes the remaining application or withdrawal decisions
before live delivery. A committed replacement/deletion result is not recreated.
It holds no historical event list. No background tasks or external resources.
An idle append is pending until a driving input; an interrupt is not permission
to start a new turn from previously unapplied input.

For an accepted queued-input Steer, the replacement `input/message` precedes
the original input's `input/deleted` in one atomic result batch. A `notHeld`
decision writes neither result nor replacement, even though its command remains
committed, and returns no Steer receipt. The service replies to an accepted Steer only
after the result batch is published; a retry after a lost reply cannot replace
the deleted input again. A candidate in `input/withdraw.steer` is not itself an
adopted input and never enters model context. The owner contract is
[Session access A.4](../../../design/contracts/session-access.md#a4-withdrawal),
and its durable form is the
[v39 format decision](../../../design/appserver/native-storage-format.md#v39-conditional-queued-input-steer).

### Handover inputs

A handover input is an `input/message` whose `handover` is `{turn_id,
request_id}`: it is addressed to one handover's continuation and is
model-readable only if that continuation consumes it. InputQueue never releases
one (it writes no `input/applied`) and does not count it in the published queue
size. It deletes one with `input/deleted` when its turn ends without consuming
it, or on arrival when its turn is not open. These are the only InputQueue
rules for it; the Loop consumes it only in the batch that starts its handover's
continuation. Its instruction is a raw block with no `user`, like Goal's
automatic inputs; clients that show the operator's words never show it as the
operator's message.

A handover input is never a [state input](../../../design/model-prompts.md#5-scopes),
so an ordering rule for state inputs leaves it last.

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
