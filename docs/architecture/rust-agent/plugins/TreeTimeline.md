# TreeTimeline

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Scope: branch. Issue: [#2024](https://github.com/arcships/dim/issues/2024).

This page is the owner contract for the branch-scope route from a line's facts to the line
directory. [Current implementation](#current-implementation) describes the code as it is today.

TreeTimeline is the branch-scope half of the line directory: it watches the facts of the line it runs in and hands the ones the tree must act on to TimelineTree. It holds no state of its own and answers no caller.

## Use and scope: responsibility split

TreeTimeline owns:

- **The route from a line's own committed facts to the tree.** TimelineTree reserves a child inside `create`'s lock, before it appends `branch/create`; this part hands the committed `branch/create` to the tree, which starts that child's build and records the create position. A `branch/end` commits this line's end; a `turn/end` is the point after which a tool end settles.
- **The release of the reservations this line's children hold.** When this line unloads, a child reservation the tree made and has not dispatched is cancelled and its waiter settled, so a parent that goes away leaves no half-open line.

Callers keep: every decision about the parent's own facts; TreeTimeline decides nothing, it routes and releases.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The tree learns a child was created on this line, with the cut it inherits | No: the fact is dispatched in this line's scope, and the tree cannot subscribe to every line's facts from the Session scope | Internal handover |
| The tree learns this line committed its end, and settles it at the turn end a tool end belongs to | No: same reason; the fact and its settlement order are the tree's | Internal handover |
| The tree learns this line ended a turn, so its children see it moved | No: the sequence is published by the tree for this line's children | Internal handover |
| Reservations this line opened and never received are cancelled when the line unloads | No: the reservation table is the tree's and the unload is this scope's event | Internal handover |

No operation is public: the handover is inside one owner, across two scopes, and adding a public face would give callers a second path to the tree.

## Caller example

None: no caller holds TreeTimeline. The tree's own directory is the only reader of what this part reports, and it reads it through the owner's internal handover.

## Own contract

### Contract dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | The line's committed facts, in order. |
| `B:Timeline` / `B:TimelineIdentity` | Creation and end settlement location and this line's id. |
| `S:TreeHandle` | The owner's weak handle to TimelineTree: the internal handover for creation, end and turn end. Weak because the tree holds this line's scope and the scope holds this part. |

### Operations

None public. The handover is three tree-internal calls — a child was created, this line ended, this line ended a turn — plus one release on unload. They are not on the data face and no other owner may call them.

### Errors

None reach a caller. A routing failure is recorded on the tree's cleanup list and surfaces in the Session's diagnostics; a child that fails to build answers its own creator through the tree's `create` reply.

### Flows

| Case | Normal path | Line unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| A child was created on this line | TimelineTree already reserved the child inside `create`'s lock and appended `branch/create`; the subscription hands that committed fact to the tree, which starts the build and records the create position | A reservation the tree made and never dispatched is cancelled; its waiter is settled | The tree's close cancels every reservation before the scopes close | TreeTimeline releases reservations; TimelineTree settles the children |
| This line committed its end | A tool end waits for the turn end; any other end settles at once | The end is already the tree's; the scope closes after its last write | An end in flight finishes before the generation's close | TimelineTree |
| This line ended a turn | The tree publishes the sequence for this line's children | Nothing held | Nothing held | TimelineTree |

## Supersedes

- The `TreeTimeline` dependency on `S:TreeCommands` in [timeline branches §5](../../../design/timeline-branches.md#5-parts): the service is the owner's weak `S:TreeHandle`.

## Client-visible changes

None: the part has no public face.

## Exceptions with reasons

- The routing crosses two scopes inside one owner: the subscription must live in the branch scope, while the reservation table and the directory are the Session's. `Weak` access to the tree keeps the branch scope from owning the tree, which the tree already holds through this line's scope.
- A reservation this line opened and never dispatched is cancelled on unload and its waiters settle; a build already in flight is awaited and publishes, and the line is then the tree's.

## Verification

Flow tests on the owner:

- `create` reserves the child under its own lock; the committed `branch/create` starts the build through this part; a parent that unloads before the reservation is dispatched cancels it and settles the waiter.
- A tool end waits for the line's turn end and settles after it; a client end settles without waiting.
- A turn end publishes the sequence this line's children read; an ended line publishes nothing further.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | TreeTimeline is one branch part and a Capability: it consumes the line's committed facts and drives the tree's writes; it holds no state of its own and writes no fact. |
| §2 scopes | Branch scope for the subscription and the unload; the reservation table and the cap lock are TimelineTree's, in `create`. |
| §3 orthogonality | It routes facts and touches no other owner's state. |
| §4 rule 1 | No command; it reacts to committed facts. |
| §4 rule 2 | The handover is a fact route; the tree writes every resulting fact. |
| §4 rule 3 | The handler registers the tree's work and returns; no I/O waits in the handler. |
| §4 rule 4 | No state is kept here. |
| §4 rule 5 | No batch. |
| §4 rule 6 | No context input is produced. |
| §4 rule 7 | It uses the owner's internal handover, not a public service. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | It acts after the fact's batch commits, as the dispatcher delivers it. |
| §7 | Every argument and reply is data and a line is an id; no object or handle crosses, except the owner's internal weak handle. Deleting the old surface and migrating every caller is one change. The owner answers its own state and facts, never another owner's record. |
| ADR 002 D1 | Branch scope: the subscription lives with the line. |
| ADR 002 D2 | No wiring plugin: the part belongs to the TimelineTree plugin. |
| ADR 002 D3 | Depends only on branch services and the tree. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | No public contract, so no language binding; the handover is Rust-internal. |

## Requirements raised to mechanism owners

None.

## Binding appendix

None: no service and no wire name. The part provides no name in the catalog.

## Current implementation

The code here still uses the pre-contract names (`TreeCommands`, `treeCommands`); the contract above is the target.

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Routes branch-local creation facts to the Session tree using the actual parent
identity, and releases pending child reservations when the parent unloads.

#### Dependencies

| Service | Purpose |
| --- | --- |
| `S:TreeCommands` | Reach the owning Session tree through its command port. |
| `B:TimelineIdentity` | Identify the parent branch. |
| `B:Dispatcher` | Consume creation events. |
| `B:Timeline` | Parent history, creation and end settlement location. |

#### Events and lifetime

Consumes `branch/create` and `branch/end` (with the next turn end of a line
that ended through a tool call), and reports every `turn/end` to the tree,
which publishes it for the line's children; tree-owned work produces the
opening, failure and end facts (see TimelineTree). The handler registers creation, not a long-running build future.
Cleanup cancels pending reservations for this parent and settles their waiters.
No independent worker or persistent tree is owned here. Reload does not by
itself replay arbitrary old create events; recovery belongs to TimelineTree.
An accepted creation need not survive parent or Session close.

#### Tests

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
