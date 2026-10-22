# ShellInput

Owner: [shell_input.rs](../../../../crates/dim-agent-capabilities/src/shell_input.rs).
Runs the commands a user types as `!command` and `!!command` in the Session's
working directory, through the Session methods `shell.run`, `shell.stop` and
`shell.list` (`sessionMethods` key `shell`). Design:
[ShellInput](../../../design/plugins/ShellInput.md).

## Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Build each command as `exec` does (`build_process_spec`) and spawn its process tree. |
| `S:SessionConfig` | The Session working directory. |
| `S:TreeCommands` | Check `branchId` at start; write a recorded run's input on that branch. |
| `S:SessionLifecycle` | `closing()` ends a run without writing; `hold()` keeps the Session from an idle unload while a run is registered. |
| `S:SessionIdentity` | Session id of the `shell.run` span, one per started run. |

## Methods

`defaults::install_local_execution` installs the plugin and records the
namespace. A process that did not install it answers `shell.*` with
`unknown_method`; Rpc answers `service_reloading` while a Session's ShellInput
generation is not active.

| Method | Result |
| --- | --- |
| `shell.run {sessionId, branchId?, runId, command, mode}` | After the run: `{runId, status, exitCode?, signal?, reason?, output, truncated, durationMs, recorded?}`; progress items `{text, skipped?}` |
| `shell.stop {sessionId, runId}` | `{stopped: true}` once that run has ended, `{stopped: false}` when it is not running; a stop for an unused `runId` is remembered, and that run then starts nothing and replies `stopped` |
| `shell.list {sessionId}` | `{runs: [{runId, branchId, mode, command, startedAt}]}` |

Refusals: `invalid_params`, `operation_interrupted` (the call's token fired
before the spawn), `service_reloading` (the generation is unloading),
`session_closed` (the Session is closing, or no hold can be taken), `shell_busy`
(a run is registered), `duplicate_run` (the `runId` started a run in this
generation, or a stop ended it first; a refused start does not use it),
`shell_unavailable` (`ProcessSpecError` code and reason), `spawn_failed`, and
the facade's `AgentError` codes for an unknown branch or closed Session. A
rejected write of a recorded run is `recorded.error {appError, message}`;
`appError` is absent only for a facade failure that has no application code on
the wire either.

## Events and lifetime

No events of its own and no Dispatcher subscription. A `record` run writes one
input after its process tree is released and its output read: an Append raw
block `<dim-shell-input kind="command" status exit? truncated>` holding
`$ <command>` and the bounded output, escaped as context-block text. It is
written once and never retried. InputQueue applies it at once and Loop consumes
it with the next model request; it starts no turn.

The run registry (the running run, the run ids used in this generation and the
run ids a stop reached before their run) is generation memory, never rebuilt. A run task belongs to the generation's
`TaskTracker`, not to the call that started it:

```text
call's token (Rpc reload,      handler returns; the call already replied
provider unload)               operation_interrupted. The run continues and
                               is recorded as after a disconnect.

ctx cancellation (generation   run task terminates the process tree, joins
unload) or Session closing()   the pipe readers and writes nothing; the
                               generation's disposer waits for the task.
```

A run is admitted before its command is built and again, under the registry
lock, in one step with its Session hold, spawn and registration. The hold is
kept in the run's registry entry, so it drops when the run task removes the run
after its last fact, before `shell.stop` or the reply sees the end; while it is
held `Session::unload_if_idle()` refuses, so an idle unload never ends a run
(idle unload R3). The build is raced against the call's token, and the second
admission reads that token, the generation token, the Session phase and the
remembered stops, so a call interrupted before the spawn starts nothing and
leaves its `runId` unused. The disposer closes the tracker under the same lock,
so no run starts after the tracker closed. The spawn holds that std mutex, which
no await holds, so `shell.list` and `shell.stop` wait at most for one fork and
exec. The write's ending check is followed by no await: a run the Session part
ended never writes, and the branch refuses a write once its own close has
started. `SessionMethods::provide` is called after the disposer is registered,
so the provider's unload, which settles the calls, runs first.

Output follows `exec`'s rules through the helpers `workspace_tools::exec`
exports: one `pump` and decoder per pipe with the 100 ms read grace after the
root exits, `capture_preview` and `truncate_preview` for the last 2000 lines
within 50 KiB, `sanitize` for progress text, `signal_name` for the reply.
Progress keeps the last 64 KiB of sanitized text since the previous item
(`tail_bytes`), counts the bytes it left out as `skipped`, and is sent at most
once per 100 ms, the rest after the pipes drain. It is best effort: C1 drops it
after the reply or under backpressure.

A root that exits on its own releases its tree: background processes it left
(`!cmd &`) keep running outside ShellInput, and their pipes close after the
grace. The run's `shell.run` span in the process log carries the Session and
branch ids, the `runId` and the outcome (`completed:<exit>`, `stopped`,
`failed`, `interrupted`), never the command or its output.

## Rule check

- §1 and ADR 002 D1–D3: one Session capability in its own module; Rpc reaches
  it through `sessionMethods` by namespace and names no ShellInput type; no core
  part depends on it.
- §4 rules 1, 2 and 4: ShellInput changes no other module's state. Its only
  Timeline effect is an ordinary Append input, written once and then owned by
  InputQueue; Rpc, InputQueue and Loop have no ShellInput handling.
- §4 rules 3, 5 and 9: no before callback and no Timeline reads; only a
  recorded result enters the Timeline, while progress is reply data of one call.
- §4 rules 6 and 7: the output reaches the model only through the consumed
  input; ShellInput uses only public services (`Branch::input` through
  `TreeCommands`, `SessionConfig`, `SessionLifecycle`, `SessionIdentity`,
  HostPlatform and `sessionMethods`).
- ADR 002 D4–D5: no configuration of its own; the exec shell is HostPlatform's.
  The commands are the user's, not an agent's tool calls, so no Approval or
  Permission applies (ADR 003 D1).

## Requirements raised to mechanism owners

None (§4 rule 8). C1 and S7 supply the method contract, per-call progress and
the command builder.

## Tests

[shell_input.rs](../../../../crates/dim-app-server/tests/transport_flow/shell_input.rs):
`shell_methods_need_the_assembly_and_check_their_requests`,
`a_private_run_replies_exec_bounded_output_and_writes_nothing`,
`one_run_per_session_until_it_ends_or_is_stopped`,
`closing_the_session_terminates_the_run`,
`an_rpc_reload_ends_the_wait_but_not_the_run`,
`a_session_closing_before_the_spawn_starts_nothing`,
`a_recorded_run_is_one_append_input_for_the_next_request`,
`a_run_outlives_its_caller_and_holds_the_session_until_recorded`,
`an_rpc_reload_before_the_spawn_starts_nothing`,
`a_run_is_admitted_again_with_its_spawn`,
`a_run_recorded_during_a_tool_batch_follows_its_result`,
`a_closed_branch_refuses_the_write_but_not_the_run`,
`reloading_shell_input_alone_ends_its_run`,
`the_caller_receives_progress_before_the_reply`,
`progress_items_are_capped_and_count_what_they_leave_out`. Feature `testing`
adds `shell_input::testing::hold_start`, which holds a run's command build open
for the race flows; production builds have no hook.

<!-- generated:plugin ShellInput — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:HostPlatform`、`S:SessionConfig`、`S:SessionIdentity`、`S:SessionLifecycle`、`S:TreeCommands` |
| 提供 | `S:SessionMethods#shell` |
| 被注入于 | — |
<!-- /generated -->
