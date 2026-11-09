# BranchTool

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Registers the Session's `branch` tool, through which one line opens, messages,
reads and ends its lines ([timeline branches §11](../../../design/timeline-branches.md#11-the-branch-tool)).
It is installed with the default execution loop; the minimal execution profile
leaves it out. The facts it emits and consumes are BranchLine's, the branch
part below.

`create` passes the optional idle notice policy to TimelineTree. The child
restores it from its opening identity before BranchLine handles turn ends.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Create a child, write and read a line, and end a line, per call. |
| `S:ToolRegistry` | Register `branch`, until this part unloads. |

## Events and lifetime

No events of its own: the tree writes `branch/create`, `branch/start` and
`branch/end`; a call writes its result, and `message` and `end` write their
inputs through `TreeCommands.input` (`end` the final message before the
command it writes). The input is one
action-based object, `{action, prompt?, at?, to?, content?, mode?, branch?,
after?, limit?}`, with `action` required and no other properties; risk stays
`session`, and Approval admits each call like any tool
([event-driven flows](../event-driven-flows.md)). A refusal is an error result
whose `structuredContent` carries a code (`branch_not_main`, `branch_not_kin`,
`branch_not_tool_line`, `branch_ended`, `branch_cannot_end`) and writes
nothing. The tools registry and the calling line's BranchLine are found per
call; the registration is withdrawn when this part unloads, and a reloaded
TimelineTree reloads it with the services it injects.

`read` first resolves the raw page’s complete-batch endpoint through `TreeCommands.page_end`, then queries only display fields within that range through `TreeCommands.query`. It does not decode model requests, tool-call inputs or tool-result content. Both the raw-event limit and continuation remain batch based. It renders one item each: input,
text, tool call, tool result and turn end. A tool result whose call lies on an
earlier page carries no name: each page is rendered on its own. A tool
result with declared effects carries them on its item, absent when the
result declared none, a path verbatim and a command cut at its 200th
character with the cut marked (session boundary A.6; the compaction digest
shares the rule). The target is the line the caller was cut from, and on
`main` a tool line it opened; with no
`after`, the read continues after the caller's last successful read of that
line, which the line's BranchLine folds from its own `tool/use` and
`tool/result` (design [§6](../../../design/timeline-branches.md#6-messages-between-lines)).

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs):
`branch_tool_reuses_capabilities_without_replaying_ancestor_inputs`;
[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`a_stop_settles_the_branch_tool_while_the_tree_still_builds_the_child`,
`a_refused_branch_call_writes_nothing`,
`a_tool_line_ends_itself_and_main_learns_it_from_the_roster`,
`a_message_reaches_its_receiver_in_the_mode_it_asked_for`,
`a_first_parent_read_starts_after_the_cut`,
`an_after_below_the_cut_is_raised_to_it`,
`a_read_without_after_continues_from_the_last_page_in_both_directions`,
`main_re_reads_a_line_from_zero_and_the_cursor_follows_that_read`,
`a_read_cursor_survives_a_restore`,
`an_after_past_the_targets_head_is_refused_and_the_cursor_stays`.

<!-- generated:plugin BranchTool — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:ToolRegistry`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## BranchLine

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Folds the line's own committed facts into its role, its open tool lines, the
state of its current turn and the line's read cursors, and writes the roster,
the parent block, the idle notice and the ended notice through
`S:TreeCommands`.

Its role (main, tool line or host line) and its parent come from
`TreeCommands.info`, which is filled from the parent's `branch/create` at
creation and at restore.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Consume the line's own facts in order after the rebuild. |
| `B:Timeline` | Read the fields the line's fold uses at one committed cutoff, including the unpublished tail. |
| `B:TimelineIdentity` | The line's own id. |
| `B:BranchLifecycle` | Wait for the branch to be Active before resolving its role and writing. |
| `S:TreeCommands` | `info` for the role and its lines; `head`, the crate-internal `turn_ends` and the parent's new-activity check for the parent block; `input` for every write. |
| `S:GuideEntries` | Register the roster and message entries on every line, until this part unloads. |

### Events and lifetime

Consumes `branch/create`, `branch/create-failed`, `branch/ended`, `branch/end`,
`input/message`, `input/consumed`, `input/deleted`, `turn/end`, `tool/use` and
`tool/result`. It writes only `input/message`, through the tree's input path.

- **Roster.** On `main`, a keyed Append of `<dim-branch-lines>` lists the tool
  lines it opened that are still open, in the order it opened them, with the
  first line of each purpose; when none is open the key is retired.
- **Parent block.** On a tool line or a host line, one keyed Append of
  `<dim-branch-parent>` on the line itself when its parent ends a turn past
  the line's read cursor for it, the head the latest block reported, and the
  line's opening, the sequence of its `branch/create` on the parent. It names
  the parent, that head and the cursor, and never carries the parent's
  content. The fold keeps the cursor of each successful read from the line's
  own `tool/use` and `tool/result`, so a replay rebuilds it. The block is
  decided from the line's state rather than the parent's signal: the parent's
  turn-end, published by TimelineTree, and the line's own write queue wake the
  writer, and consuming, deleting or interrupting a held block queues the same
  decision. An idle line is not woken, and `main` gets none. A child whose
  inherited prefix holds a live parent block, meaning a nested line, gets one
  more block in its opening input, naming its own parent at the cut;
  TimelineTree writes it with the identity block
  (design [§7.1.1](../../../design/timeline-branches.md#711-the-parent-block-branchparent)).
- **Idle notice.** On a tool line, one Steer to its parent at a `turn/end` that
  neither steered the parent in that turn, called `end`, nor left a driving
  input pending. A steer counts only once its tool result says the call was
  accepted: a refused or denied call wrote nothing to the parent, which would
  otherwise wait on silence.
- **Ended notice.** On `main`, the roster update and `<dim-branch-ended>` in one
  Steer, for any end of a tool line other than the line's own `end`.

The rebuild pages through branch lifecycle fields, input identities and modes,
roster and parent blocks, branch tool routing fields, and result success/cursor
fields. It keeps the resulting line state, not the input pages. It reads no
user content, unrelated tool arguments or tool result content, and writes
nothing; each write follows a live event, except one heal of a load that writes
a roster a crash left missing and at most one parent block a restore finds due.
A
restart can lose an idle notice: the notice is decided on live events only
(design [§6.2](../../../design/timeline-branches.md#62-known-costs)). It
registers with `Dispatcher::consume` and writes from one owned task, which
rechecks the fold before writing.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`main_lists_its_open_tool_lines_under_one_key`,
`a_configured_roster_closing_reaches_the_roster`,
`an_idle_line_tells_main_once_when_its_turn_ends_without_a_word`,
`a_tool_line_ends_itself_and_main_learns_it_from_the_roster`,
`an_end_from_outside_a_tool_line_wakes_main_with_a_notice`,
`the_roster_and_the_identity_replay_after_a_compaction`,
`an_idle_line_is_told_its_parent_moved_without_being_woken`,
`a_running_line_gets_the_block_with_its_next_request_once`,
`two_parent_turns_while_the_line_is_idle_write_one_block`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_consumed`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_deleted`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_interrupted`,
`a_restore_writes_a_due_parent_block_once_and_none_without_movement`,
`an_ending_line_and_main_get_no_parent_block`,
`the_parent_block_replays_after_a_compaction`,
`the_creating_call_is_written_before_the_line_was_opened`,
`a_host_line_cut_at_the_start_is_not_told_what_happened_before_it`,
`a_host_line_cut_at_the_start_is_told_when_main_acts_after_it`,
`a_host_line_cut_before_it_was_opened_is_not_told_about_the_gap`,
`a_nested_line_is_told_its_own_parent_at_open`,
`a_childs_first_system_is_its_parents_at_the_cut`.

<!-- generated:plugin BranchLine — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:GuideEntries`、`S:TreeCommands` |
| 提供 | `B:BranchLine` |
| 被注入于 | — |
<!-- /generated -->
