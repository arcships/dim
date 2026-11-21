# TimelineTree

The TimelineTree plugin registers TimelineTree in every Session and TreeTimeline in every branch.

The single-line display read also belongs to TimelineTree. The directory itself
is read without history I/O; display details are requested separately. It reads the
creation prompt/source and end fact by position only when detail is requested. It omits the
optional inherited user-message count instead of querying the historical prefix. Ended prefixes use the tree's stored branch chain.
Restore selects lifecycle kind/positions first. Closed lines retain directory metadata without
reading creation/end bodies; unfinished closes read the terminal data they must settle, and
only active lines read their opening source. Parent-child adjacency orders the directory once.
A Session with one line is on its root without a query; a Session with more reads the root's
newest `branch/default` or `session/forked` through the kind index to find the default branch.
The App Server only maps these owner-produced properties into its wire fields.
`TreeCommands.page_end` resolves a raw-event page endpoint using atomic-batch metadata through the live Timeline or the ended line’s store. It decodes no event body and has no wire binding. The default branch's answer and its write are `TreeCommands` methods too: `default_branch()` answers the line the Session's conversation is on, restored from the root's newest `branch/default` or `session/forked`, and `set_default_branch(id, source)` admits an open conversation line whose chain is not ending, then writes `branch/default` on the root. Its refusals are `branch_already_default`, `branch_not_selectable` and `branch_target_protected`, and it writes nothing for any of them. A move queues its transition on every participant that is not the root, so a line owes its note from the moment it can take part, and signals every line; each line's own BranchLine drains its queue and writes the one status note its role earns; a load whose latest `branch:line` block disagrees with the tree writes the missing note once, and on the tree's current default branch a block whose text differs from the note this code writes is replaced once.

## TimelineTree

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Owns Session branches, reservations and asynchronous creation/restore/close,
the default branch, and a line's end: the identity block in every child's opening input, the parent
block a nested line is told at open, the `info.source` a line was opened with,
the one `branch/default` fact on the root, `branch/end` to `branch/ended` on the parent, and the restore rule that leaves
an ended line closed
([timeline branches §5–§10](../../../design/timeline-branches.md#10-ending-a-line)).
Provides the TreeCommands/TimelineTreeControl ports. The `branch` tool is
[BranchTool](BranchTool.md)'s. Its model-facing texts — every child's identity
block and the branch markers — with their classes are listed on
[timeline branches §12.4](../../../design/timeline-branches.md#124-texts-and-their-classes).

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineFactory` | Build branch capability scopes. |
| `S:SessionIdentity` | Identify the persisted Session. |
| `S:dyn TimelineStore` | Native binding of the Session [`timelineStore` data face](../../../design/resource-faces.md#5-store-and-timelinestore); persist/load branch directories and Timeline facts through the same backend operations. |
| `S:GuideEntries` | Register the `<dim-branch-line>` and `<dim-branch-parent>` entries for the Session, until this part unloads. |

### Events and lifetime

Produces `branch/create`, `branch/create-failed`, `branch/start`, `branch/resume`,
`branch/end`, `branch/ended` and `branch/default`, and initial child input, through tree-owned
operations. TreeTimeline consumes creation on the parent and an end on the
ending line. `TreeCommands::end`
checks only the target: it exists, it is open, and it is neither the root, the
default branch nor an ancestor of it; a line
that ended writes nothing and answers `branch_ended`. `info` answers for an
ended line too, and `page` reads an open or ended line from the store at its
head, rebuilding stored request bodies. `batch_end`
checks a cursor against that line's atomic batch boundaries.
`query` selects a line's own facts or its inherited chain: it forwards to the Timeline while open and uses the ended head and retained directory after closing, with the property query's fixed cutoff, selection and continuation. Chain scope accepts only published facts (`include_publishing: false`), bounds every ancestor at its inherited cut and never reaches siblings or later parent facts. Continuation retains the original cutoff and query, including when a page ends in an ancestor. Own-line queries retain their existing publishing-tail contract.

A child's opening input
is the identity block, then, when the prefix it inherits holds a live roster,
one block that retires that key, and when it holds a live parent block, one
parent block naming the line that opened it at the cut; both keys are read
over the chain the child
inherits, never from the rendered history, while the child opens, as a resumed
line's facts are, and a failed read fails the create like any other opening
failure (branch/create-failed). Each line's node publishes the
sequence of its latest `turn/end`, which the line's BranchLine waits on for the
parent block (design
[§7.1.1](../../../design/timeline-branches.md#711-the-parent-block-branchparent)). `TreeCommands` also
answers whether a line's parent has new activity past a sequence with an
existence query over operator inputs and tool uses. Opening and end checks
select lifecycle fields; inherited parent state selects the last matching
keyed input and reads only its keys and retire flags. Candidate branches remain private until opening
receipts/dispatch fences and initial input acceptance succeed. Creation retains
its fixed ancestry cut even if a waiter disappears. One opening budget requests
cancellation; rollback and accepted host work must still settle. Cleanup closes
the tree, cancels reservations, joins builds and closes branch contexts. Restore settles interrupted creates without replaying external work. It selects each child's terminal kind and end position, preserving that end identity even if a fork has no own history for the child. Only a line that will mount reads its creation source at the known anchor. Accepted ancestry cuts use the directory directly; only a new cut is checked against context during creation. Tool-call
ancestry cuts cannot split required result relationships. Client detach is not
Session close, and closing is not deletion of persisted history.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`delayed_create_uses_its_anchor_and_survives_an_abandoned_waiter`,
`candidates_remain_private_until_the_initial_input_is_accepted`,
`session_close_cancels_an_active_build_and_waits_for_rollback`,
`ending_a_line_ends_its_children_first_and_records_one_result_each`,
`a_restart_completes_an_end_without_its_result`,
`a_host_line_opens_with_its_identity_block_and_the_creator_prompt`,
`a_nested_line_is_told_its_own_parent_at_open`,
`a_child_cut_after_a_roster_retires_it_and_one_cut_before_gets_nothing`,
`a_line_reports_the_source_its_create_recorded_open_or_restored`,
`the_default_branch_starts_at_the_root_and_survives_a_restart`,
`a_restore_reads_one_fact_for_a_multi_line_session_and_none_for_a_single_line`,
`ending_the_root_the_default_branch_or_an_ancestor_is_refused`,
`a_user_opened_line_records_its_own_source`,
`a_default_branch_that_was_a_tool_line_stops_sending_its_idle_notice`,
`a_switched_line_keeps_its_identity_after_a_compaction`.

<!-- generated:plugin TimelineTree — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:GuideEntries`、`S:SessionIdentity`、`S:TimelineFactory`、`S:dyn TimelineStore` |
| 提供 | `S:TimelineTree`、`S:TreeCommands`、`S:dyn HostDispatch#treeCommands`、`S:dyn TimelineTreeControl` |
| 被注入于 | `BranchLine`、`BranchTool`、`InputMethods`、`InputQueue`、`LoopMethods`、`PromptSuggestion`、`Questions`、`Recall`、`ShellInput`、`ToolMethods`、`TreeTimeline`、`Wait`、`WorkspaceTools` |
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
| `B:Timeline` | Parent history, creation and end settlement location. |

### Events and lifetime

Consumes `branch/create` and `branch/end` (with the next turn end of a line
that ended through a tool call), and reports every `turn/end` to the tree,
which publishes it for the line's children; tree-owned work produces the
opening, failure and end facts (see TimelineTree). The handler registers creation, not a long-running build future.
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
