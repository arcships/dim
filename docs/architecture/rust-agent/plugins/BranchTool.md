# BranchTool

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Registers the Session's `branch` tool, which forks the calling branch through
[TimelineTree](TimelineTree.md). It is installed with the default execution
loop; the minimal execution profile leaves it out.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Create the child branch from the calling branch, per call. |
| `S:ToolRegistry` | Register `branch`, until this part unloads. |

## Events and lifetime

No events of its own: the tree writes `branch/create` and the child's
`branch/start`, and the tool writes its result. The tool's input is
`{prompt, at?}`, risk `session`; Approval admits it like any tool
([event-driven flows](../event-driven-flows.md)). The registration is withdrawn
when this part unloads, and a reloaded TimelineTree reloads it with the
TreeCommands it injects.

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs):
`branch_tool_reuses_capabilities_without_replaying_ancestor_inputs`;
[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`a_stop_settles_the_branch_tool_while_the_tree_still_builds_the_child`.

<!-- generated:plugin BranchTool — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:ToolRegistry`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->
