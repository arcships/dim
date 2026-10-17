# TimelineTree

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Owns Session branches, reservations and asynchronous creation/restore/close;
contributes the branch tool and TreeCommands/TimelineTreeControl ports, and the
Session service `treeCommands` that binds TreeCommands for other languages
([tree_binding.rs](../../../../crates/dim-agent-capabilities/src/tree_binding.rs),
[C5](../../../design/branch-typescript.md)).

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineFactory` | Build branch capability scopes. |
| `S:SessionIdentity` | Identify the persisted Session. |
| `S:dyn TimelineStore` | Persist/load branch directories and timeline facts. |

## Events and lifetime

Produces `branch/create`, `branch/create-failed`, `branch/start`, `branch/resume`
and initial child input through tree-owned operations. TreeTimeline consumes
creation on each parent. Candidate branches remain private until opening
receipts/dispatch fences and initial input acceptance succeed. Creation retains
its fixed ancestry cut even if a waiter disappears. One opening budget requests
cancellation; rollback and accepted host work must still settle. Cleanup closes
the tree, cancels reservations, joins builds and closes branch contexts. Restore
settles interrupted creates without blindly replaying external work. Tool-call
ancestry cuts cannot split required result relationships. Client detach is not
Session close, and closing is not deletion of persisted history.

## Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`delayed_create_uses_its_anchor_and_survives_an_abandoned_waiter`,
`candidates_remain_private_until_the_initial_input_is_accepted`,
`session_close_cancels_an_active_build_and_waits_for_rollback`.

<!-- generated:plugin TimelineTree — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionIdentity`、`S:TimelineFactory`、`S:dyn TimelineStore` |
| 提供 | `S:TimelineTree`、`S:ToolContribution#branch`、`S:TreeCommands`、`S:dyn HostDispatch#treeCommands`、`S:dyn TimelineTreeControl` |
| 被注入于 | `InputMethods`、`ShellInput`、`TreeTimeline`、`Wait`、`WorkspaceTools` |
<!-- /generated -->
