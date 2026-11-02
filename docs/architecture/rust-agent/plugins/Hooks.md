# Hooks

The Hooks plugin runs user commands at Session and turn lifecycle points. The
document format, the per-event protocol and the acceptance criteria live in the
[Hooks design](../../../design/plugins/Hooks.md); this page records how the
local-execution parts are assembled and what each one owns.

Two Session/branch parts are in scope here: HooksSession, which binds the
runtime, decides `SessionStart` and answers `UserPromptSubmit`, and
HooksBranch, which observes one branch and runs the handlers of its hook
points. The configuration owner,
HooksSettings, is an App Server plugin: it registers the `hooks`,
`hooks-projects` and `hooks-plugins` Settings sources, the `hooks.*` management
methods, the `P:HooksService` that PluginManager's owner operations call, and
the `HookSources` Session part that binds `S:HookSourcesHost`. Their assembly
is in the generated App Server services page.

A plugin's document is registered through `P:HooksService`, kept as the
`{dir, hooks}` entry of `hooks-plugins` and read after the user's and the
project's sources as `plugin:<id>`; its commands read the entry's `dir` as
`DIM_PLUGIN_ROOT`. The user's and the project's commands never set it, and
`CLAUDE_PLUGIN_ROOT` and `GROK_PLUGIN_ROOT` stay out of every command's
environment.

## HooksSession

Owner: [hooks/runner.rs](../../../../crates/dim-agent-capabilities/src/hooks/runner.rs).
Provides the Session's single `S:HooksRuntime`, which reads the effective hook
sources for the Session's working directory, builds the payload of one hook
point, and runs its commands. It also registers the submit check `hooks` with
the Session's `submitChecks` ([InputQueue](InputQueue.md)), which the
operator-input entries call before writing: the check reads the sources, runs
the matching `UserPromptSubmit` handlers concurrently, and answers with the
first block in catalog order or the contexts as text blocks after the
operator's own content. Handlers after the first block are terminated. A
failure, a timeout and a cancellation are the handler's `onFailure`; an unload
settles a pending check by `onFailure` before the part is withdrawn, and an
interruption of the entry's own call cancels the check, so nothing is written.

The runtime reads sources when a hook point is evaluated, never at Session
start: a change to the user's `hooks.json` or to a project document applies to
the next hook point, and the catalog is recomputed rather than cached. Reading
is a `S:HookSourcesHost` call, which the App Server binds to Settings and
FileStorage; a Session whose host binds no sources (the scripted model
assembly) sees an empty catalog. `DIMCODE_HOME` comes from the same port, so a
Session started by a product daemon passes the daemon's product home into every
hook command and a Session without one sets nothing.

Every process the runtime starts is owned by `ctx.effect` through its
`TaskTracker`: a synchronous handler is awaited by the evaluating part, an
`async` one is spawned and settled before the part unloads, and the branch's
cancellation token terminates its process tree on close.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:HookSourcesHost` | Read the effective documents of the user, the workspace and the plugins. |
| `P:HostPlatform` | Build each command's process spec, environment and process tree. |
| `S:SessionConfig` | Pass the working directory and permission facts to the payload. |
| `S:SessionIdentity` | Name the Session in the payload and `DIM_SESSION_ID`. |
| `S:SubmitChecks` | Register `hooks`, the `UserPromptSubmit` check the operator-input entries call. |

<!-- generated:plugin HooksSession — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:HostPlatform`、`S:HookSourcesHost`、`S:SessionConfig`、`S:SessionIdentity`、`S:SubmitChecks` |
| 提供 | `S:HooksRuntime` |
| 被注入于 | `HooksBranch` |
<!-- /generated -->

## HooksBranch

Owner: [hooks/runner.rs](../../../../crates/dim-agent-capabilities/src/hooks/runner.rs).
Folds the branch's committed events into a consumed view and evaluates the hook
points that follow from them: `PostToolUse` and `PostToolUseFailure` after a
settled tool result, `PermissionRequest` and `Notification` at an approval
request, and `Stop` at a completed turn.

It also registers the advisor `hooks` with the branch's Permission
([Permission](Permission.md), Hooks A.6.1), which is how `PreToolUse` reaches
the admission: `Permission::admit_decision` asks it while it decides a call,
and the advisor reads the sources, runs the matching `PreToolUse` handlers
concurrently, and answers with the combination A.5 fixes — any deny, then any
ask, then any allow, and the first deny in catalog order is the reason, so the
handlers after it are terminated while the ones before it run to completion.
The advice is not authority: Permission folds it with its own decision, a deny
of its own is not asked about, and Approval's recheck after an answer asks
nothing. A deny or ask reason reads `Hook <source>: <reason>`. While the part
is mounted the branch's calls wait for their advice; an unload drops a pending
advice as no opinion, so Permission decides alone.

The dispatcher handler only folds the event and starts the work; it never
runs a command and never waits. A branch task drains the queued points in
order, so handlers of one point start together, the point's combined output is
written once, and a later point waits for the earlier one. The exception is
`PermissionRequest`: its run is a task of its own, spawned when the live
request is folded, so it waits for no earlier point and delays no later one.
Its token is registered before the task starts, and the live
`approval/respond` or `approval/cancelled` cancels it. A run that finds its
token cancelled, before it starts or after the source read, starts no command.
Before writing, the branch rechecks the consumed view: output whose triggering
fact an interrupt or a newer *consumed* operator input has overtaken is
dropped, and `SessionStart` output is also dropped once a turn has started on
the branch, because its block must come before the first turn. An input counts as the operator's when it carries `user` content and
its first block is not a `<dim-…>` source marker (session access A.1–A.2), and
only its `input/consumed` counts, so a queued input or another source's input
drops nothing.

Commands run outside the branch's commit path and never take the commit lock.
Each handler gets a timeout; the process tree is terminated when it expires,
which is reported as a failure of that handler and honored by `onFailure`.
Standard output is capped at 64 KiB for the context contribution, standard
error at 2 KiB for a failure or blocking reason, and a context block at 16 KiB.
The README of the local profile carries the process facts; the timeout,
`onFailure`, `async` and de-duplication rules are the design's.

`Stop` writes a `<dim-hooks kind="follow-up">` input only when at least one
handler blocks, carrying the block reasons and then the contexts in catalog
order; a context alone contributes nothing. The input queues a new turn,
bounded at three consecutive follow-ups, and a turn driven by an operator
input resets the count. The follow-up is recognized from its own
`dim-hooks kind="follow-up"` marker as the view folds the branch, so the
identity and the count are rebuilt from the Timeline when the part or the
Session restores, and another source's queued input is never counted.

`SessionStart` is derived on `main` from the folded view: it runs only when no
`turn/start` and no Session-start block was seen; it runs at most once per
`main` because the block it writes is what the derivation checks for. The
block is keyed `hooks:session-start/<n>`, which is what keeps it in the
compacted context baseline.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Approval` | Answer a `PermissionRequest` decision for the pending admission. |
| `B:Permission` | Register the `hooks` advisor the admission decision asks (`PreToolUse`). |
| `S:HooksRuntime` | Read sources, build payloads and run the point's handlers. |
| `B:BranchLifecycle` | Close the branch's jobs and settle its processes. |
| `B:TimelineIdentity` | Name the branch in the payload and in the main-branch rule. |
| `B:Dispatcher` | Follow the events that open each hook point. |
| `B:Timeline` | Read the history for `SessionStart` and append output. |
| `S:ToolRegistry` | Read a tool's declaration for its matcher alias when the branch adopted no tool part. |

<!-- generated:plugin HooksBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Approval`、`B:BranchLifecycle`、`B:Dispatcher`、`B:Permission`、`B:Timeline`、`B:TimelineIdentity`、`S:HooksRuntime`、`S:ToolRegistry` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## Diagnostics

A handler that fails, times out or is cancelled is reported with its source
(`user`, `project` or `plugin:<id>`), its event, its exit status and duration,
and the bounded standard-error excerpt. A hook's own output never appears in
diagnostics beyond that excerpt, and a handler whose document does not parse is
reported by `hooks.catalog` rather than at the hook point.

## Tests

[hooks_flow.rs](../../../../crates/dim-agent-capabilities/tests/hooks_flow.rs):
`session_start_writes_one_keyed_block_per_hook_and_the_model_sees_it`,
`session_start_context_is_dropped_once_the_first_turn_has_run`,
`session_start_context_is_dropped_by_an_interrupt`,
`a_second_session_start_does_not_run_the_hooks_again`,
`the_session_start_block_is_still_in_context_after_compaction`,
`post_tool_use_follows_the_protocol_for_success_block_and_failure`,
`a_hook_context_cannot_close_its_runtime_block`,
`a_prompt_context_cannot_close_its_runtime_block`,
`post_tool_use_failure_runs_only_for_error_results`,
`a_timeout_is_a_failure_and_a_blocking_failure_still_answers`,
`identical_commands_at_one_point_run_once`,
`an_identical_command_in_the_user_and_project_sources_runs_once`,
`identical_commands_from_two_plugins_run_with_their_own_roots`,
`stop_follow_ups_are_bounded_at_three_and_flag_the_driven_turns`,
`a_context_only_stop_writes_no_follow_up_and_starts_no_turn`,
`a_stop_follow_up_carries_the_block_reason_then_the_context`,
`a_stop_follow_up_is_recognized_when_its_reason_carries_markup`,
`only_the_follow_ups_turn_reports_stop_hook_active`,
`permission_request_answers_through_approval`,
`a_failed_permission_request_with_block_denies_the_approval`,
`a_failed_permission_request_with_continue_answers_allow`,
`a_timed_out_permission_request_with_block_denies_the_approval`,
`a_user_answer_cancels_the_hook_and_does_not_delay_the_next_point`,
`a_permission_request_does_not_wait_for_a_running_post_tool_hook`,
`notification_runs_on_an_approval_and_writes_nothing`,
`hook_output_is_dropped_after_an_interrupt`,
`a_queued_operator_input_does_not_drop_the_turns_post_tool_output`,
`a_source_input_with_user_content_does_not_drop_the_output`,
`an_async_handler_does_not_delay_the_turn_or_the_next_point`,
`a_hook_sees_its_environment_and_no_plugin_root`,
`a_project_source_runs_nothing_until_the_user_enables_it`,
`a_plugin_source_runs_like_any_other_source`,
`disable_all_hooks_turns_every_source_off`,
`a_blocked_steer_writes_nothing_and_the_old_input_still_applies`,
`the_prompt_context_follows_the_operator_text_and_keeps_it_operator_input`,
`a_source_input_runs_no_prompt_hook`,
`an_unload_settles_a_pending_prompt_check_by_on_failure`,
`an_unload_with_the_default_on_failure_writes_the_input`,
`the_handlers_after_the_first_block_are_terminated`,
`two_proceeding_handlers_keep_their_contexts_in_catalog_order`,
`a_failure_finishing_after_a_later_result_blocks_by_its_own_handler`,
`a_pre_tool_use_deny_fails_one_call_of_a_batch_and_runs_the_others`,
`a_pre_tool_use_allow_runs_a_call_the_preset_would_ask_about`,
`a_pre_tool_use_ask_asks_the_user_like_any_other_ask`,
`a_command_rule_deny_runs_no_pre_tool_use_hook`,
`the_recheck_after_an_answer_runs_no_pre_tool_use_hook`,
`a_pasted_bash_matcher_runs_for_exec_and_reads_the_command`,
`a_pasted_permission_matcher_runs_without_an_adopted_tool_part`,
`a_pasted_post_tool_matchers_run_for_exec_and_read_without_an_adopted_tool_part`,
`a_pasted_matcher_does_not_select_the_other_tool`,
`bash_selects_a_registered_tool_with_risk_execute_process`,
`matchers_keep_selecting_tools_with_an_adopted_tool_part`,
`an_adopted_tool_part_alias_selects_at_pre_tool_use`,
`an_adopted_tool_part_alias_selects_at_permission_request`,
`a_tool_batch_keeps_the_alias_of_the_declaration_it_folded_under`,
`the_first_deny_in_catalog_order_is_the_reason`,
`an_interrupt_while_pre_tool_use_runs_cancels_the_call_and_ends_the_command`,
`a_hooks_unload_drops_a_pending_pre_tool_use_advice`;
`runner.rs`'s `pre_tool_use_outcomes_follow_the_protocol` pins A.4's outcomes
for the event, exit 2 and the `onFailure` mapping included, and that
`updatedInput` never rewrites the call;
`a_settled_tool_call_leaves_nothing_behind_in_the_view` pins
that a settled call's input does not stay in the folded view;
`a_json_array_on_stdout_is_prompt_context` pins that plain text, other JSON
values included, is prompt context (A.3's exit-0 row);
`the_follow_up_state_is_rebuilt_from_the_branch` pins that the follow-up's
identity and consecutive count are rebuilt from the branch, not carried in
memory; `a_deleted_operator_input_is_forgotten` pins that a withdrawn operator
input leaves the record and moves nothing;
`the_live_subscription_follows_exactly_the_kinds_the_folds_match` pins that the
live subscription follows every event kind the two folds handle, `input/deleted`
included;
`a_live_request_registers_its_token_before_its_task_starts`,
`an_answer_folded_before_the_task_starts_runs_no_permission_command` and
`an_answer_folded_during_the_read_runs_no_permission_command` pin that an
answered request starts no command;
`exit_two_is_a_failure_at_session_start_and_notification` pins that exit 2 at
the two observation hooks is the reported failure, not a block;
`a_session_start_hook_reads_only_its_context` pins that `SessionStart` reads
`additionalContext` alone, not another event's `decision` and `reason`;
`a_failure_keeps_a_bounded_stderr_excerpt` pins the diagnostic's excerpt; and
`hooks_settings.rs`'s `a_hook_point_reads_sources_without_authorizing_a_project`
pins that reading a Session's sources never authorizes a workspace.
[hooks_settings_flow.rs](../../../../crates/dim-app-server/tests/hooks_settings_flow.rs):
`catalog_reports_sources_states_and_never_reads_foreign_hook_files`,
`project_hooks_stay_disabled_until_the_user_enables_them_and_edits_apply`,
`a_user_document_is_saved_with_revisions_and_reports_disable_all_hooks`,
`plugin_documents_are_registered_listed_and_kept_by_their_owner`;
[transport_flow/plugin_manager.rs](../../../../crates/dim-app-server/tests/transport_flow/plugin_manager.rs):
`plugin_hooks_run_with_their_plugin_root_and_follow_the_record` and
`a_refused_plugin_hook_document_stops_only_its_own_kind` pin the owner
operations end to end, and `plugin_hooks_follow_the_hooks_owner` pins that an
absent owner skips only its kind and that turning HooksSettings on registers
that kind;
[host_platform_flow.rs](../../../../crates/dim-agent-capabilities/tests/host_platform_flow.rs):
`spawn_with_stdin_delivers_the_bytes_and_closes_the_pipe`.
