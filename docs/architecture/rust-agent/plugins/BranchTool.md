# BranchTool

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Registers the Session's `branch` tool, through which one line opens, messages,
reads and ends its lines ([timeline branches §11](../../../design/timeline-branches.md#11-the-branch-tool)).
It is installed with the default execution loop; the minimal execution profile
leaves it out. The facts it emits and consumes are BranchLine's, the branch
part below.

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

`read` renders one page of the line's events, one item each: input, text, tool
call, tool result and turn end. A tool result whose call lies on an earlier
page carries no name: each page is rendered on its own.

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs):
`branch_tool_reuses_capabilities_without_replaying_ancestor_inputs`;
[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`a_stop_settles_the_branch_tool_while_the_tree_still_builds_the_child`,
`a_refused_branch_call_writes_nothing`,
`a_tool_line_ends_itself_and_main_learns_it_from_the_roster`,
`a_message_reaches_its_receiver_in_the_mode_it_asked_for`.

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
Folds the line's own committed facts into its role, its open tool lines and the
state of its current turn, and writes the roster, the idle notice and the ended
notice through `S:TreeCommands`.

Its role (main, tool line or host line) and its parent come from
`TreeCommands.info`, which is filled from the parent's `branch/create` at
creation and at restore.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Replay the line's own facts, then live events. |
| `B:TimelineIdentity` | The line's own id. |
| `B:BranchLifecycle` | Wait for the branch to be Active before resolving its role and writing. |
| `S:TreeCommands` | `info` for the role and its lines; `input` for every write. |

### Events and lifetime

Consumes `branch/create`, `branch/create-failed`, `branch/ended`, `branch/end`,
`input/message`, `input/consumed`, `input/deleted`, `turn/end`, `tool/use` and
`tool/result`. It writes only `input/message`, through the tree's input path.

- **Roster.** On `main`, a keyed Append of `<dim-branch-lines>` lists the tool
  lines it opened that are still open, in the order it opened them, with the
  first line of each purpose; when none is open the key is retired.
- **Idle notice.** On a tool line, one Steer to its parent at a `turn/end` that
  neither steered the parent in that turn, called `end`, nor left a driving
  input pending. A steer counts only once its tool result says the call was
  accepted: a refused or denied call wrote nothing to the parent, which would
  otherwise wait on silence.
- **Ended notice.** On `main`, the roster update and `<dim-branch-ended>` in one
  Steer, for any end of a tool line other than the line's own `end`.

The replay rebuilds the fold and writes nothing; each write follows a live
event, except one heal of a load that writes a roster a crash left missing. A
restart can lose an idle notice: the notice is decided on live events only
(design [§6.2](../../../design/timeline-branches.md#62-known-costs)). It
registers with `Dispatcher::follow` and writes from one owned task, which
rechecks the fold before writing.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`main_lists_its_open_tool_lines_under_one_key`,
`an_idle_line_tells_main_once_when_its_turn_ends_without_a_word`,
`a_tool_line_ends_itself_and_main_learns_it_from_the_roster`,
`an_end_from_outside_a_tool_line_wakes_main_with_a_notice`,
`the_roster_and_the_identity_replay_after_a_compaction`.

<!-- generated:plugin BranchLine — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:TimelineIdentity`、`S:TreeCommands` |
| 提供 | `B:BranchLine` |
| 被注入于 | — |
<!-- /generated -->
