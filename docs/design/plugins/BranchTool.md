# BranchTool

Scope: Session. Issue: #2024.

BranchTool is the model's `branch` tool: the only way a line opens, messages, reads or ends another line. It owns the tool's action menu, its kin rules and the shape of its answer. The current implementation is described in the [BranchTool plugin page](../../architecture/rust-agent/plugins/BranchTool.md).

TimelineTree owns the directory and the lifecycle; BranchLine owns the calling line's view; the lines' input owners accept writes. BranchTool decides, per call, what the calling line may do and how the answer reads.

## Use and scope: responsibility split

BranchTool owns:

- **The action menu.** `create`, `message`, `read`, `end`, the fields each takes, and the refusal when a field belongs to another action.
- **The kin rule.** A line messages and reads the line it was cut from and the tool lines it opened; a plugin line follows its creator's protocol and opens nothing; a line the user opened is not on a roster and is ended by the user.
- **The read page.** Which ranges a read covers, where it continues from the line's cursor, the item shapes and the effects a tool result declares.
- **The model-facing text** of every refusal and every page.

Callers keep: nothing — the tool is the model's entrance. The Session owns the tool registry and the assembly decides whether the tool is installed.

| Requirement (model intent) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Open a line to work on a part of the task | No: the cut is the calling request's own start, which the tool reads from its execution context | `timelineTree.create` |
| Send a line a message or a last word | No: the write belongs to the target line's input owner; the tool decides the kin rule | Target line's input owner by id, with the mode the tool chose |
| Read what a line did, continuing where the last read stopped | No: the continuation is the calling line's cursor, owned by BranchLine; the page boundaries and the rendering are the tool's | `branchLine.view`, `timelineTree.read` |
| End the calling tool line with its last word | No: the end and the last word are one settled operation | `timelineTree.end` |
| Learn what the calling line may do | No: role and parent are BranchLine's fold | `branchLine.view` |
| Check that a target belongs to the pair before messaging or reading it | No: the record's parent and source decide it | `timelineTree.line` with `{line}` |

## Caller example

The model calls `branch`; the tool maps one action onto the owner operations and answers one page or one refusal.

```rust
// create: the cut is this request's own start, read from the execution context.
let cut = self.request_start(execution).await?;      // business: the calling request's start
let line = tree.create(CreateLine {
    parent: execution.branch_id.clone(),
    cut,
    prompt,
    source: BranchSource::Tool { turn_id, request_id, call_id },
    idle_notice: input.idle_notice,
}, cancelled).await
    .map_err(refusal_from_line_error)?;

// message: the kin rule is the tool's; the write is the target line's owner.
if !kin(&own, &target) {
    return refused("branch_not_kin");
}
line_owner::<InputQueue>(target.line.clone())?
    .submit(InputMessageData::message(&own.line, &target.line, content, mode))
    .await?;

// read: the cursor is the calling line's, the page is the tool's rendering.
let own = line_owner::<BranchLine>(&execution.branch_id)?.view().await?;
let after = input.after.unwrap_or_else(|| {
    own.cursors.iter().find(|c| c.target == target.line).map(|c| c.cursor).unwrap_or(cut)
});
let page = tree.read(ReadLines {
    line: target.line.clone(),
    from: Some(after + 1),
    through: Some(target.head),
    fields: READ_FIELDS.to_vec(),
    limit: Some(input.limit.unwrap_or(READ_PAGE_LIMIT)),
}).await
    .map_err(refusal_from_line_error)?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` | Register the `branch` tool, until this part unloads. |
| `S:TimelineTree` | `create`, `end`, `read` and `line`. |
| `line_owner::<BranchLine>` | The calling line's role, end state, read cursors and held parent block (`view`). |

### Operations

One tool, one request object, no service:

| Field | Meaning |
| --- | --- |
| `action` | `create`, `message`, `read` or `end`; required, no other property allowed |
| `prompt` | `create`: what the new line is for |
| `at` | `create`: the cut; absent means this request's own start |
| `idle_notice` | `create`: `steer` (default) or `none` |
| `to`, `content`, `mode` | `message`: the target, the text and the mode (`append` or `steer`) |
| `branch`, `after`, `limit` | `read`: the target, the continuation and the page size |
| `content` | `end`: the line's last word |

Every reply is text; `read` carries a structured `{items, nextCursor, done}`. A refusal is an error result whose `structuredContent` carries one code and no write happens.

### Errors

The tool's coded refusals are the accepted set ([timeline branches §11](../../design/timeline-branches.md#11-the-branch-tool)) plus the argument error; every other ending of the four actions is a failed result without a code, so the model-visible result shape does not change. The route adds no code of its own: `NoLine` from `line_owner` is reported as `branch_ended`, `SessionClosing` as a failed result without a code, and a Session part never receives `NoSession` ([#2127](https://github.com/arcships/dim/issues/2127); [TimelineTree](../../architecture/rust-agent/plugins/TimelineTree.md#session-parts-reaching-a-lines-owners)).

Refusals, with `structuredContent.code`:

| Code | What the model does next |
| --- | --- |
| `branch_not_opener` | Only the root and the conversation line open lines; move the conversation or ask one of those |
| `branch_limit_reached` | End an open tool line first |
| `branch_not_kin` | Address the line it was cut from, or a line it opened |
| `branch_not_tool_line` | A plugin line follows its creator's protocol; a plugin line reads only the line it was cut from |
| `branch_ended` | The target's own end is recorded, or the calling line's own end is: read the line instead. Not retryable; an owed last word is delivered by restore |
| `branch_cannot_end` | Not retryable as is: only a tool line ends itself, and not while its chain holds the conversation line |
| `invalid` | Fix the action's fields |

Endings without a code, reported as a failed result:

| Ending | What the model does next |
| --- | --- |
| The read's `after` is past the target's head | Re-read `line` and start at or below its head; retry once |
| The page's cutoff is inside an atomic batch | Start at the record's head, which ends a batch |
| A continuation arrived without its `after` | Resend `after` with the current record |
| The tool's own start is not a usable cut | Report it; do not retry |
| The target's id is not a line of this Session | Report `branch_not_kin` at the pair check; never retry the same call |
| The tree answers `branch_target_protected` (a cascade is ending the target) | Read the line and report the state; do not retry the same end |
| `SessionClosing` or a closing Session | Stop; nothing more happens in this generation |
| An admitted creation cancelled by its caller | The line publishes without a waiter and the call has no result |

### Flows

| Action | Normal path | Caller cancels | Session closes | Settled by |
| --- | --- | --- | --- | --- |
| `create` | Read this request's start as the cut, admit and build through the tree, answer the line id | A cancel before admission writes nothing; after admission the line publishes and the caller sees `cancelled` from its own tool call | Refused `branch_ended`/`closed` | TimelineTree |
| `message` | Check the pair, write into the target's queue, answer the target id | The write is one commit; a cancelled call writes nothing | Refused `closed` | The target line's input owner |
| `read` | Resolve the target record, the cursor and the head, read one page, render items | Nothing to release | Refused `closed` | TimelineTree, BranchLine |
| `end` | Check that the caller is a tool line and that its own end is not yet recorded, then call the tree's `end` with the last word; a line that already recorded its own end answers `branch_ended` and an owed last word is delivered by restore | The end settles even if the call stops watching | Refused `closed` | TimelineTree |

## Supersedes

- The `branch` tool's description in [timeline branches §11](../../design/timeline-branches.md#11-the-branch-tool) as far as it names `TreeCommands` for a message and a page: the tool calls `timelineTree` and `line_owner`, as below.

## Client-visible changes

None: the tool's action menu, fields, result shape and refusal codes are unchanged.

## Exceptions with reasons

- The tool reads the calling request's own start to cut the new line: the request identity is the tool execution's context, not a line fact, so no owner can supply it.
- The rendered page repeats a tool result's declared effects and cuts a command at its 200th character: the rendering is model-facing text and belongs to the tool that reads it.

## Verification

Flow tests on the tool:

- A root and the conversation line open a line; a tool line and a plugin line answer `branch_not_opener` with no write.
- The Session's cap counts open tool lines outside the conversation line's chain; a creation failure releases the slot.
- A message reaches its receiver in the requested mode; a sibling or a grandparent answers `branch_not_kin` with nothing written.
- A read of the line the caller was cut from starts at the cut; a read without `after` continues from the line's cursor; an `after` past the head answers an error and leaves the cursor unchanged.
- `end` on a line that is not the default line withholds no final word; a line whose chain holds the conversation line answers `branch_cannot_end`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BranchTool is one Session part and not a Capability: it registers a tool and folds nothing. |
| §2 scopes | Session scope for the registration; each call acts on the calling line's id. |
| §3 orthogonality | It reads the directory and the calling line's state and writes through owners. |
| §4 rule 1 | A refused call writes nothing; an accepted one lands in the target owner's operation. |
| §4 rule 2 | It writes no fact of its own; the owners it calls do. |
| §4 rule 3 | The tool returns after its work; waits belong to the owners it calls. |
| §4 rule 4 | It keeps no state between calls except what BranchLine publishes. |
| §4 rule 5 | It reads whole batches through the tree's read. |
| §4 rule 6 | The page is the tool result, the only text the model reads. |
| §4 rule 7 | It uses the owners' operations; it holds no scope, node or queue. |
| §4 rule 8 | One requirement is raised with TimelineTree's: resolving a line id to a branch-scope service ([#2127](https://github.com/arcships/dim/issues/2127)). |
| §4 rule 9 | It writes no fact. |
| §7 | Every argument and reply is data and a line is an id; no object or handle crosses, except the owner's internal weak handle. Deleting the old surface and migrating every caller is one change. The owner answers its own state and facts, never another owner's record. |
| ADR 002 D1 | Session scope: one registration for every line. |
| ADR 002 D2 | No wiring plugin: the tool belongs to the TimelineTree plugin's feature. |
| ADR 002 D3 | Depends only on the registry, the tree and BranchLine. |
| ADR 002 D4 | The profile decides whether the tool is installed. |
| ADR 002 D5 | The tool's request and reply are data; the Rust binding is the appendix. |
| ADR 027 | The tool reaches BranchLine and a target's input owner with `line_owner::<T>(line)`. |

## Requirements raised to mechanism owners

The same one as TimelineTree's: the tool reaches a line's input owner and BranchLine with `line_owner::<T>(line)`, which needs the runtime's id routing ([#2127](https://github.com/arcships/dim/issues/2127); [SessionScope](https://github.com/arcships/dim/pull/2093) states the Session side).

## Binding appendix

No service and no wire name. The request is the tool's input object; the refusal codes are its data. The Rust side is the tool handler registered with `ToolRegistry`.
