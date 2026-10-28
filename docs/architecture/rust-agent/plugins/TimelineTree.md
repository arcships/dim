# TimelineTree

The TimelineTree plugin registers TimelineTree in every Session and TreeTimeline in every branch.

## TimelineTree

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Owns Session branches, reservations and asynchronous creation/restore/close;
provides the TreeCommands/TimelineTreeControl ports. The `branch` tool is
[BranchTool](BranchTool.md)'s.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineFactory` | Build branch capability scopes. |
| `S:SessionIdentity` | Identify the persisted Session. |
| `S:dyn TimelineStore` | Persist/load branch directories and timeline facts. |

### Events and lifetime

Produces `branch/create`, `branch/create-failed`, `branch/start`, `branch/resume`
and initial child input through tree-owned operations. TreeTimeline consumes
creation on each parent. Candidate branches remain private until opening
receipts/dispatch fences and initial input acceptance succeed. Creation retains
its fixed ancestry cut even if a waiter disappears. One opening budget requests
cancellation; rollback and accepted host work must still settle. Cleanup closes
the tree, cancels reservations, joins builds and closes branch contexts. Restore
settles interrupted creates without blindly replaying external work; it reads a
branch's failed creates while the branch opens, from the history its
capabilities rebuild from, and settles a child's creation from its first events
([restore-shared-read §17](../../../design/appserver/restore-shared-read.md#17-timelinetrees-restore-facts)). Tool-call
ancestry cuts cannot split required result relationships. Client detach is not
Session close, and closing is not deletion of persisted history.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`delayed_create_uses_its_anchor_and_survives_an_abandoned_waiter`,
`candidates_remain_private_until_the_initial_input_is_accepted`,
`session_close_cancels_an_active_build_and_waits_for_rollback`.

<!-- generated:plugin TimelineTree — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionIdentity`、`S:TimelineFactory`、`S:dyn TimelineStore` |
| 提供 | `S:TimelineTree`、`S:TreeCommands`、`S:dyn HostDispatch#treeCommands`、`S:dyn TimelineTreeControl` |
| 被注入于 | `BranchTool`、`InputMethods`、`ShellInput`、`TreeTimeline`、`Wait`、`WorkspaceTools` |
<!-- /generated -->

## TreeTimeline

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Routes branch-local creation facts to the Session tree using the actual parent
identity, and releases pending child reservations when the parent unloads.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Reach the owning Session tree through its command port. |
| `B:TimelineIdentity` | Identify the parent branch. |
| `B:Dispatcher` | Consume creation events. |
| `B:Timeline` | Parent history and creation settlement location. |

### Events and lifetime

Consumes `branch/create`; tree-owned work produces opening/failure facts (see
TimelineTree). The handler registers creation, not a long-running build future.
Cleanup cancels pending reservations for this parent and settles their waiters.
No independent worker or persistent tree is owned here. Reload does not by
itself replay arbitrary old create events; recovery belongs to TimelineTree.
An accepted creation need not survive parent or Session close.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`parent_close_waits_for_dispatch_before_releasing_branch_reservations`,
`close_releases_an_unconsumed_create_without_waiting_for_dispatch`.

<!-- generated:plugin TreeTimeline — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->
