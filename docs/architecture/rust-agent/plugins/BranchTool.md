# BranchTool

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Registers the Session's `branch` tool, through which one line opens, messages,
reads and ends its lines ([timeline branches §11](../../../design/timeline-branches.md#11-the-branch-tool)).
It is installed with the default execution loop; the minimal execution profile
leaves it out. The facts it emits and consumes are BranchLine's, the branch
part below.

`create` passes the optional idle notice policy to TimelineTree. The child
restores it from its opening identity before BranchLine handles turn ends. `create` admits
two callers and refuses every other line: the root, in every state, and the current
default branch. The root's right never changes, which is what lets a user ask it for a
line while another line holds the role. The cap is a total over the Session and counts
the open tool lines that are not on the protected path, so a line the user keeps
continuing on costs no slot.

`end` ends the calling line and nothing else, so it guards the caller as well as the
target: only a tool line ends itself, a plugin line follows its own protocol, and a line
the user opened is ended by the user. A line that is the default branch, or that has a
descendant which is, is refused, because ending a line ends every line below it.

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
whose `structuredContent` carries a code (`branch_not_opener`, `branch_limit_reached`,
`branch_not_kin`, `branch_not_tool_line`, `branch_ended`, `branch_cannot_end`) and writes
nothing. The tools registry and the calling line's BranchLine are found per
call; the registration is withdrawn when this part unloads, and a reloaded
TimelineTree reloads it with the services it injects.

`read` first resolves the raw page’s complete-batch endpoint through `TreeCommands.page_end`, then queries only display fields within that range through `TreeCommands.query`. It does not decode model requests, tool-call inputs or tool-result content. Both the raw-event limit and continuation remain batch based. It renders one item each: input,
text, tool call, tool result and turn end. A tool result whose call lies on an
earlier page carries no name: each page is rendered on its own. A tool
result with declared effects carries them on its item, absent when the
result declared none, a path verbatim and a command cut at its 200th
character with the cut marked (session boundary A.6; the compaction digest
shares the rule). The target is the line the caller was cut from or a line the caller opened; with no
`after`, the read continues after the caller's last successful read of that
line, which BranchLine retains from live results and otherwise reads for that
specific target from its own `step/tool-use` and `step/tool-result` (design [§6](../../../design/timeline-branches.md#6-messages-between-lines)).

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
Folds the line's own committed facts into its role, the tool lines it opened,
the state of its current turn and the line's read cursors, and writes its roster,
the parent block, the idle notice, the ended notice and a status note through
`S:TreeCommands`.

Its role and its parent come from
`TreeCommands.info`, which is filled from the parent's `branch/create` at
creation and at restore.

### Data face

The branch-scoped `branchLine` face exposes the owner's existing read operations:
`resolved()` returns `[role, parent]` or `null`, where role is `main` (the root), `tool`, `user` or
`plugin`; `defaultBranch()` reports whether the line is the current default branch; `ending()` reports whether the line committed its own `branch/end`;
`cursor(target)` returns its latest successful read cursor for that target or
`null`. Cursor read failures use the standard `AgentError` name, variant code
and display message on the wire; native error source identity is not serialized.
The native binding and foreign dispatcher call these same operations. Reads retain the current cached
role/cursor behavior and load only the requested target's cursor when needed;
they do not read or rebuild all history. Writer lifecycle and event settlement
are unchanged. See [service faces](../../../design/service-faces.md).

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Consume the line's own facts in order after the rebuild. |
| `B:Timeline` | Fix the committed mount cutoff. |
| `B:TimelineIdentity` | The line's own id. |
| `B:BranchLifecycle` | Wait for the branch to be Active before writing. |
| `S:SessionLifecycle` | Wait for the restored child directory to be published before roster repair. |
| `S:TreeCommands` | `info` for the role and its lines; `head`, the crate-internal `turn_ends` and the parent's new-activity check for the parent block; `input` for every write. |
| `S:GuideEntries` | Register the roster and message entries on every line, until this part unloads. |

### Events and lifetime

Consumes `branch/create`, `branch/create-failed`, `branch/ended`, `branch/end`,
`input/message`, `input/consumed`, `input/deleted`, `turn/end`, `step/tool-use` and
`step/tool-result`. It writes only `input/message`, through the tree's input path.

- **Roster.** On a line that has opened tool lines, a keyed Append under `branch:roster` of `<dim-branch-lines>` lists the lines it opened with the tool that are still open, in the order it opened them, with the
  first line of each purpose; when none is open the key is retired. A plugin line is never on a roster, and neither is a line the user opened: it stays in the Session's directory and coordinates its own tool lines.
- **Parent block.** On a line that is not the root, one keyed Append of
  `<dim-branch-parent>` on the line itself when its opener ends a turn past
  the line's read cursor for it, the head the latest block reported, and the
  line's opening, the sequence of its `branch/create` on the parent. It names
  the parent, that head and the cursor, and never carries the parent's
  content. The fold keeps the cursor of each successful read from the line's
  own `step/tool-use` and `step/tool-result`, so a replay rebuilds it. The block is
  decided from the line's state rather than the parent's signal: the parent's
  turn-end, published by TimelineTree, and the line's own write queue wake the
  writer, and consuming, deleting or interrupting a held block queues the same
  decision. An idle line is not woken, and the root gets none. A child whose
  inherited prefix holds a live parent block, meaning a nested line, gets one
  more block in its opening input, naming its own parent at the cut;
  TimelineTree writes it with the identity block
  (design [§7.1.1](../../../design/timeline-branches.md#711-the-parent-block-branchparent)).
- **Idle notice.** On a line opened by the tool, one Steer to its opener at a `turn/end` that
  neither steered the opener in that turn, called `end`, nor left a driving
  input pending, and only while the line is not the current default branch. A steer counts only once its tool result says the call was
  accepted: a refused or denied call wrote nothing to the parent, which would
  otherwise wait on silence.
- **Ended notice.** On the opener, the roster update and `<dim-branch-ended>` in one
  Steer, for any end of a tool line it opened other than that line's own `end`.
- **Status note.** On the new default branch and on the former one, one keyed Append of
  `<dim-branch-line>` under `branch:line`, carrying `default`, restating the identity it replaces
  and then the rules that hold while the line is, or is no longer, the default branch. The key is
  the identity's, so a compaction replays the note and the line still knows which line it is. The
  note states no rule another line could contradict: the protection of the default branch's chain
  is decided when `end` is called, from the tree. The root gets no note, because neither of its
  rules changes with the role, so a note to it would say nothing.

BranchLine mounts at the committed Dispatcher cutoff without reading historical events. It starts its writer after both branch and Session publication so all current child directory entries are available. Roster repair reads known current creation positions and the last roster block. Parent notices read the last parent block and the requested parent's read cursor. Old turn progress is not restored; a recovery turn end does not emit an idle notice. Read cursors are loaded for one target when used, and neither ordinary input history nor all tool results are retained. No recovery event is written.

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
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:GuideEntries`、`S:SessionLifecycle`、`S:TreeCommands` |
| 提供 | `B:BranchLine`、`B:dyn HostDispatch#branchLine` |
| 被注入于 | — |
<!-- /generated -->
