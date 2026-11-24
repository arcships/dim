# TreeTimeline

Scope: branch. Issue: #2024.

TreeTimeline is the branch-scope half of the line directory: it watches the facts of the line it runs in and hands the ones the tree must act on to TimelineTree. It holds no state of its own and answers no caller. The current implementation is described in the [TreeTimeline section of the plugin page](../../architecture/rust-agent/plugins/TimelineTree.md#treetimeline).

## Use and scope: responsibility split

TreeTimeline owns:

- **The route from a line's own facts to the tree.** A `branch/create` on this line opens the child's reservation; a `branch/end` commits this line's end; a `turn/end` is the point after which a tool end settles.
- **The release of this line's reservations.** When this line unloads, the children it reserved and never received are cancelled and their waiters settled, so a parent that goes away leaves no half-open line.

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

### Dependencies

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
| A child is created on this line | The subscription hands the fact to the tree; the tree reserves the child and later builds it | Reservations this line opened and never received are cancelled; their waiters are settled | The tree's close cancels every reservation before the scopes close | TreeTimeline releases reservations; TimelineTree settles the children |
| This line committed its end | A tool end waits for the turn end; any other end settles at once | The end is already the tree's; the scope closes after its last write | An end in flight finishes before the generation's close | TimelineTree |
| This line ended a turn | The tree publishes the sequence for this line's children | Nothing held | Nothing held | TimelineTree |

## Supersedes

- The `TreeTimeline` dependency on `S:TreeCommands` in [timeline branches §5](../../design/timeline-branches.md#5-parts): the service is the owner's weak `S:TreeHandle`.

## Client-visible changes

None: the part has no public face.

## Exceptions with reasons

- The routing crosses two scopes inside one owner: the subscription must live in the branch scope, while the reservation table and the directory are the Session's. `Weak` access to the tree keeps the branch scope from owning the tree, which the tree already holds through this line's scope.
- A reservation this line opened and never dispatched is cancelled on unload and its waiters settle; a build already in flight is awaited and publishes, and the line is then the tree's.

## Verification

Flow tests on the owner:

- A `branch/create` on this line reserves the child; a parent that unloads before the child is published cancels the reservation and settles the waiter.
- A tool end waits for the line's turn end and settles after it; a client end settles without waiting.
- A turn end publishes the sequence this line's children read; an ended line publishes nothing further.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | TreeTimeline is one branch part and a Capability: it consumes the line's committed facts and drives the tree's writes; it holds no state of its own and writes no fact. |
| §2 scopes | Branch scope for the subscription and the unload; the reservation table is the Session's. |
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
