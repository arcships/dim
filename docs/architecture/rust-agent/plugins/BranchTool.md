# BranchTool

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Scope: Session. Issue: [#2024](https://github.com/arcships/dim/issues/2024).

The sections through [Binding appendix](#binding-appendix) are the owner contract for the model's
`branch` tool. [Current implementation](#current-implementation) describes the code as it is today.
BranchLine is its own owner with [its own contract](BranchLine.md).

BranchTool is the model's `branch` tool: the only way a line opens, messages, reads or ends another line. It owns the tool's action menu, its kin rules and the shape of its answer. TimelineTree owns the directory and the lifecycle; BranchLine owns the calling line's view; the lines' input owners accept writes. BranchTool decides, per call, what the calling line may do and how the answer reads.

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
let own = line_owner::<BranchLine>(&execution.branch_id)?.view(Some(target.clone())).await?;
let after = input.after.unwrap_or_else(|| {
    own.cursors.iter().find(|c| c.target == target.line).map(|c| c.cursor).unwrap_or(cut)
});
let page = tree.read(ReadLines {
    line: target.line.clone(),
    from: Some(after + 1),
    through: Some(target.head),
    fields: Some(READ_FIELDS.to_vec()),
    limit: Some(input.limit.unwrap_or(READ_PAGE_LIMIT)),
}).await
    .map_err(refusal_from_line_error)?;
```

## Own contract

### Contract dependencies

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
| `idleNotice` | `create`: `steer` (default) or `none` |
| `to`, `content`, `mode` | `message`: the target, the text and the mode (`append` or `steer`) |
| `branch`, `after`, `limit` | `read`: the target, the continuation and the page size |
| `content` | `end`: the line's last word |

Every reply is text; `read` carries a structured `{items, nextCursor, done}`. A refusal is an error result whose `structuredContent` carries `{error, message}`, where `error` is the code below and `message` the text; no write happens. An argument error is a failed result with no code, as today (`Invalid branch arguments: …`).

### Errors

The tool's coded refusals are the accepted set ([timeline branches §11](../../../design/timeline-branches.md#11-the-branch-tool)); an argument error and every other ending are failed results without a code, so the model-visible result shape does not change. The pair check reads `line(id)` first: `unknown` there becomes `branch_not_kin`, never a separate code. A `NoLine` after the pair check succeeded means the target's instance closed in between, which each action reports as `branch_ended`. `SessionClosing` is a failed result without a code, and a Session part never receives `NoSession` ([#2127](https://github.com/arcships/dim/issues/2127); [TimelineTree](TimelineTree.md#session-parts-reaching-a-lines-owners)).

Refusals, with the code in `structuredContent.error`:

| Code | Actions | What the model does next |
| --- | --- | --- |
| `branch_not_opener` | `create` | Only the root and the conversation line open lines; move the conversation or ask one of those |
| `branch_limit_reached` | `create` | End an open tool line first |
| `branch_not_kin` | `message`, `read` | Address the line it was cut from, or a line it opened |
| `branch_not_tool_line` | `message`, `read` | A plugin line follows its creator's protocol; a plugin line reads only the line it was cut from |
| `branch_ended` | `create`, `message`, `read`, `end` | The target's own end is recorded, or the calling line's own end is: read the line instead. Not retryable; an owed last word is delivered by restore |
| `branch_cannot_end` | `end` | Not retryable as is: only a tool line ends itself, and not while its chain holds the conversation line |

Endings without a code, reported as a failed result:

| Ending | Actions | What the model does next |
| --- | --- | --- |
| The read's `after` is past the target's head | `read` | Re-read `line` and start at or below its head; retry once |
| The page's cutoff is inside an atomic batch | `read` | Start at the record's head, which ends a batch |
| A continuation arrived without its `after` | `read` | Resend `after` with the current record |
| The tool's own start is not a usable cut | `create` | Report it; do not retry |
| The parent or the target is not a line of this Session | `create`, `message`, `read` | Report `branch_not_kin` at the pair check; never retry the same call |
| The tree answers `branch_target_protected` (a cascade is ending the target) | `end` | Read the line and report the state; do not retry the same end |
| `SessionClosing` or a closing Session | all | Stop; nothing more happens in this generation |
| An admitted creation cancelled by its caller | `create` | The line publishes without a waiter and the call has no result |

### Flows

| Action | Normal path | Caller cancels | Session closes | Settled by |
| --- | --- | --- | --- | --- |
| `create` | Read this request's start as the cut, admit and build through the tree, answer the line id | A cancel before admission writes nothing; after admission the line publishes, the call has no result, and the caller sees `cancelled` from its own tool call | `branch_ended` when the parent ended; a failed result without a code when the Session is closing | TimelineTree |
| `message` | Check the pair, write into the target's queue, answer the target id | The write is one commit; a cancelled call writes nothing | A failed result without a code | The target line's input owner |
| `read` | Resolve the target record, the cursor and the head, read one page, render items | Nothing to release | A failed result without a code | TimelineTree, BranchLine |
| `end` | Check that the caller is a tool line and that its own end is not yet recorded, then call the tree's `end` with the last word; a line that already recorded its own end answers `branch_ended` and an owed last word is delivered by restore | The end settles even if the call stops watching | A failed result without a code | TimelineTree |

## Supersedes

- [Timeline branches §11](../../../design/timeline-branches.md#11-the-branch-tool)'s **Read** paragraph, which now points at this page's contract.

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

## Current implementation

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` | Register `branch`, until this part unloads. |
| `S:TreeCommands` | Create a child, write and read a line, and end a line, per call. |

## Events and lifetime

No events of its own: the tree writes `branch/create`, `branch/start` and
`branch/end`; a call writes its result, and `message` and `end` write their
inputs through the target line's input owner, and `end` carries the final message on
`branch/end` ([the contract](#own-contract)). The input is one
action-based object, `{action, prompt?, at?, to?, content?, mode?, branch?,
after?, limit?}`, with `action` required and no other properties; risk stays
`session`, and Approval admits each call like any tool
([event-driven flows](../event-driven-flows.md)). A refusal is an error result
whose `structuredContent` carries a code (`branch_not_opener`, `branch_limit_reached`,
`branch_not_kin`, `branch_not_tool_line`, `branch_ended`, `branch_cannot_end`) and writes
nothing. `branch_not_tool_line` names the line that is not a tool line: the
target when a caller messages or reads a plugin line it opened, and the caller
when a plugin line messages the line it was cut from. The tools registry and
the calling line's BranchLine are found per
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
`the_root_opens_a_line_while_another_line_is_default`,
`the_default_branch_opens_a_line`,
`a_former_default_branch_keeps_its_lines_and_opens_none`,
`a_plugin_line_never_opens_a_line`,
`a_tool_line_still_cannot_open_a_line`,
`the_cap_does_not_count_the_default_branch_or_its_ancestors`,
`a_switch_over_the_cap_closes_nothing`,
`a_switch_that_frees_a_slot_admits_the_next_open`,
`the_cap_is_refused_under_the_tree_lock`,
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
