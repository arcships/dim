# Timeline branches

Status: Accepted ([#427](https://github.com/arcships/dim/issues/427)). This document holds both the requirements and the design: §1–§3 answer #427's questions, and §4 onward is the design. Scope: the TimelineTree plugin (TimelineTree, TreeTimeline, BranchTool, plus a new branch part for BranchTool), one App Server built-in method, two core events, the CLI TUI, and a rewrite of [design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义). Language: Rust. Chinese: [中文](../zh/design/timeline-branches.md).

## 1. What branches are for

A **branch** is a line of the same agent inside one Session. The Session is one subject, the "I": its identity, its interaction surface with the user, its capabilities and its workspace. A branch is one line of that subject's behavior.

A child starts from its parent's history at a cut. Every event up to the cut is the child's own history as well, and those rows never change. After the cut, the child reads only its own events ([timeline.rs](../../crates/dim-agent-runtime/src/core/timeline.rs)).

Lines share facts, not context:

- Each line assembles its own context from its own facts.
- Each line compacts on its own.
- A line influences another line only by writing input to that line's Timeline.

The parent–child relation records where a history came from. It is also the only relation along which lines talk. It does not mean control, and it does not mean that a result is owed.

Answers to #427:

| # | Question | Answer |
| --- | --- | --- |
| 1 | Which scenarios, in what order | Three, in order (§3): plugin lines (delivered), lines the agent opens from main (this design), and lines the user opens from a history point and then compares and adopts (later) |
| 2 | Branch versus Collaboration member versus fork | §2. A branch is the same agent. A member or a fork is another Session |
| 3 | When a branch finishes, who needs its result, and in what form | No line owes another a result. Lines tell each other what matters while they work (§6). A line the agent opened ends with a final message to main (§10). A plugin line follows its creator's protocol, for example Pursuit's conclusion |
| 4 | When is a branch over, and is it visible afterwards | When it ends itself, its opener ends it, or the user ends it (§10). An ended line stays in the branch directory. Its history stays readable to clients and to main |
| 5 | What users see and do | Follow any line, talk to it, answer its questions and approvals, and end it (§9, §14). Compare and adopt come with scenario 3 |
| 6 | What "done" means | The acceptance flows (§20) pass, and the evaluation scenarios (§12.3) have been run locally on real models and reported in the implementing PR. The release demo ([#439](https://github.com/arcships/dim/issues/439)) can show it |

## 2. Branch, Collaboration member, fork

| | Branch | Collaboration member (subagent) | Fork |
| --- | --- | --- | --- |
| What it is | Another line of the same agent, in the same Session | Another agent with its own profile, in its own Session | A new, independent Session copied from `main` |
| History | Its parent's history up to the cut, then its own | Starts from its task, without the leader's history | `main`'s history up to a rest point, copied ([fork](appserver/session-deletion-and-fork.md)) |
| Identity and capabilities | The Session's: the same system, tools, model, permission and workspace | Its own profile: instructions, tools and model | A copy of the configuration, independent from then on |
| Relation | Parent and child write input to each other while they work. There is no task contract and no control | The leader gives it a task, and the result returns automatically. Members cannot delegate | None after the copy |
| Use it when | The same agent works in parallel on the same facts | The work needs another identity, another profile or a context of its own | Work should continue elsewhere from a past point without touching this Session |

The rule from design rules §2 still holds. When the work needs a different identity or different capabilities, it becomes a new Session (a member or a fork), never a branch.

## 3. Scenarios

| # | Scenario | Status |
| --- | --- | --- |
| S1 | **Plugin lines.** A plugin opens a line under its own design. Example: [Pursuit](plugins/Pursuit.md)'s evaluator, a host-sourced child cut at the worker's turn end, whose conclusion goes back as input (implemented, [#422](https://github.com/arcships/dim/issues/422); `runtime/plugins/goal`) | Delivered. This design adds the identity block (§7.1). Goal's recognition and end of the evaluator change with it ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) |
| S2 | **The agent opens lines from main.** Examples: test two hypotheses for a bug on two lines while main keeps reading; implement on main while a line reviews each change as it lands; research an API or one area of the code while main continues; try a second approach on separate files | This design |
| S3 | **The user opens a line from a history point, compares it, adopts it.** `session.branch_create` already opens host lines. With this design the user can follow those lines, talk to them and end them. Compare and adopt need a design of their own: what adopting a line means for main's context and for a shared workspace | Later, after this design |

These needs are not branches:

| Need | Why it is not a branch | Where it belongs |
| --- | --- | --- |
| Delegate to a specialist with its own instructions | It is another identity | Collaboration member |
| Judge whether to approve a tool call | It is a decision about one action, not a line of work | A capability with a host port ([ADR 003](../adr/003-approval-scope.md)) |
| Rewind main to an earlier point | It changes how main sees its own history; it is not a new line | A separate requirement. Today the answer is `session.fork` |
| Make edits in parallel, in isolation | Lines share the Session's workspace, and no per-line workspace exists | A fork, or a member with its own working directory |

## 4. Tree and roles

- **Depth.** The architecture allows lines at any depth. `session.branch_create` takes any parent. Goal accepts any `branchId`, so an evaluator opened under a child is a grandchild.
- **Kinship.** Parent and child is the only relation along which lines communicate. A parent and each of its children write input to each other. Siblings and more distant relatives do not (§6). The rule covers one line acting on another, which here means the `branch` tool. A capability that decides on its own is a capability affecting a branch, and this rule does not limit it ([design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义)). Examples are Goal writing an evaluation result, and a plugin opening a line at any cut.
- **The agent's tool.** The `branch` tool opens lines only from `main`, the root line, which has no parent. The lines it opens are leaves: they cannot open lines of their own. The agent therefore has one coordinator, `main`, and every line the tool opened talks with `main`.
- **Sources.** Every line has a source, recorded in its parent's `branch/create`:
  - A **tool line** was opened by the `branch` tool.
  - A **host line** was opened by a plugin (`TreeCommands.create`) or a client (`session.branch_create`). Its creator owns its protocol.

  The source can be read as `TimelineInfo.source`, returned by `TreeCommands.list` and `info` and absent on `main` (§5). For a host line it is `{kind: "host", operation_id}`: the opener's opaque tag, which the core never interprets. An opener finds its own lines by its own operation-id convention, for example Goal's `goal:evaluate:` prefix. Clients' lines carry App Server's ids.
- **Reach of the tool.** The tool's `message`, `read` and `end` actions work only on tool lines. A host line is not on main's roster, and the tool neither messages it nor ends it.

| Line | Opens lines with the tool | Messages, with the tool | Reads, with the tool | Ended by, by convention |
| --- | --- | --- | --- | --- |
| `main` | Yes | Its tool lines | Its tool lines, open or ended | Never; the Session's close is not an end |
| Tool line | No (the tool result refuses) | `main`, its parent | — | Itself (`end`), the user, or an ancestor's end |
| Host line | No | — (its creator's protocol) | — | Its creator (Goal ends its evaluator, [Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)), the user, or an ancestor's end |

Who ends which line is a convention among callers. The runtime checks only the target: it exists, it is open, and it is not `main` (§10).

## 5. Parts

Every part belongs to the TimelineTree plugin ([timeline_tree.rs](../../crates/dim-agent-capabilities/src/timeline_tree.rs)), and each sits in the scope that owns its state ([ADR 002](../adr/002-plugin-scope.md) D1).

| Part | Scope | Capability | Owns in this design |
| --- | --- | --- | --- |
| TimelineTree | Session | No | The line lifecycle: the identity block in every child's opening input, `TimelineInfo.source`, the end of a line (`branch/end` → `branch/ended`), refusing input once a line is ending, the restore rule, and reads of an ended line's history |
| TreeTimeline | Branch | Yes | Routes `branch/end` to TimelineTree, as it routes `branch/create` today. For an end issued by a tool call, it also routes the turn end that the close waits for |
| BranchTool | Session | No | The action-based `branch` tool (§11). It finds the calling line's BranchLine per call, the way WorkspaceTools finds FileReads |
| **BranchLine** (new) | Branch | Yes | One line's view of its lines. Its role (main, tool line or host line) and its parent come from `TreeCommands.info(own id)`. From its own facts it folds its open tool lines and the state of its current turn. It writes the roster, the idle notice and the ended notice |

BranchLine injects `B:Dispatcher`, `B:TimelineIdentity`, `B:BranchLifecycle` and `S:TreeCommands`, and provides `B:BranchLine`.

It decides the way Goal does: from consumed state, through an owned write task that rechecks before writing, and only on live events after its load cutoff. A reload therefore writes nothing twice, with one exception: a load whose own history ends with a live roster key the fold does not match writes the roster once, because a restart that completed an ended line (§20.7) recorded `branch/ended` before `main`'s replay, so no live event follows it (§6.2).

**Line sources.** TimelineTree fills `TimelineInfo.source` from the parent's `branch/create`:

- at creation, from the create itself;
- at restore, from the resumed parent's history read.

That read already exists. A resumed parent's build reads its history up to its head and reports the creates it recorded as failed (`Opening::Resume`'s `failed_creates` in [timeline_tree.rs](../../crates/dim-agent-capabilities/src/timeline_tree.rs)). It now also reports each create's source, and each `branch/ended`, which the restore rule uses (§10). The stored directory row does not change ([storage format](appserver/native-storage-format.md#427-v20-格式决定)).

## 6. Messages between lines

A message is an `input/message` on the receiver's Timeline, written through `TreeCommands.input`. No new input path is added. A message carries one raw block, marked by its producer and without a key, and the mode of [InputQueue](../architecture/rust-agent/plugins/InputQueue.md):

| Mode | Effect on the receiver | When to use it |
| --- | --- | --- |
| `append` (default) | Rides with the receiver's next model request. It never starts a turn and does not wake `wait` | Progress or context the receiver should have at its next step |
| `steer` | Enters the receiver's current turn at its next step, or starts a turn if the receiver is idle. It wakes `wait` | The receiver must act on it now, or the receiver is idle |

Queue is not exposed. The only Queue input one line gives another is the opening input.

These are Collaboration's message modes and words (`append`, `steer`), with the same meaning, because both are InputQueue's modes.

- **Marker.** `<dim-branch-message from="C" to="P">`. The receiver tells a message from the user's words by its marker. RPC accepts only operator content, never blocks, so a client cannot forge a message.
- **Allowed pairs.** `main` and each of its open tool lines, in both directions. Any other pair is refused through the tool result, with nothing written: siblings, host lines, ended lines.
- **Read.** `read {branch, after?, limit?}` lets `main` read one of its tool lines, open or ended. It reads complete committed batches from the store through `TreeCommands.page`, and renders them the way Collaboration's `read` renders a member's page: inputs, text, tool calls and results, and turn ends, with `nextCursor`. A line does not read `main`. What a line knows of `main` after the cut is what `main` sends it, so `main` decides what each line needs, and no line pulls another line's whole stream into its own context.
- **No caps.** There is no limit on the number of lines, messages or steers. A message loop ends through the prompt rules (§12), through a line ending itself, or through the user (interrupt or end). Why this is safe, and who would own limits, is in §6.1.
- **Idle notice.** When a tool line's turn ends, BranchLine on that line writes one steer to `main` (`<dim-branch-idle>`, §12.1), unless one of the following holds:
  - the turn already sent `main` a steer message;
  - the turn called `end`;
  - the line has a driving input pending.

  This way a line that stops without telling `main` never leaves `main` waiting on silence, and a line that already steered `main` does not notify it twice. The notice carries the turn's status (`completed`, `failed` or `cancelled`).
- **Waiting.** `main` waits for its lines with the existing `wait` tool. A steer message, an idle notice or an ended notice wakes it ([wait.rs](../../crates/dim-agent-capabilities/src/wait.rs)).

### 6.1 Resources and limits

Leaving caps out is safe because a line adds no new kind of resource:

- A line's work is ordinary turns of the same Session, under the same provider, Permission and Approval.
- A line runs at most one turn at a time. Loop holds one turn per line ([agent_loop.rs:106](../../crates/dim-agent-capabilities/src/agent_loop.rs)).
- A message is one bounded `input/message` written by a tool call, so the model's own output bounds it.
- RPC admission is bounded per connection ([architecture](../architecture/README.md)).
- Collaboration has no member cap either.

If limits are ever needed, each one already has an owner, and none of them needs a mechanism change:

| Limit | Owner |
| --- | --- |
| Open lines | BranchTool refuses `create` based on main's roster, which BranchLine folds |
| Steer frequency | BranchLine on the sender counts the steer messages on its own Timeline |
| Model cost or concurrency across a Session | The model-call layer. This limit is not specific to branches: it applies equally to members and forks |

Until then, the safeguards are:

- the prompt rules;
- ended lines refusing messages;
- the user's end and interrupt;
- the flooding, over-splitting and ping-pong scenarios in §12.3, measured locally and reported in the implementing PR, never as CI thresholds.

What open lines do cost:

- An open line keeps its loaded parts.
- Every open line is reloaded on restart. Neither Session close nor idle unload ends a line.
- Open tool lines grow main's roster, which lists only open tool lines. Each end of a tool line from outside it adds one notice on `main`.

A tool line that never ends is the responsibility of its own protocol (`end`) and of the user, and main's roster keeps it visible. Goal ends Pursuit's evaluators ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)).

### 6.2 Known costs

- **A restart can lose an idle notice.** If the process stops between a tool line's turn end and its idle notice, the notice is never written, because BranchLine decides only on live events after load. Pursuit §4.2 leaves the same window open. `main` still has the line on its roster and can `read` it, and the user sees it idle.
- **A restart can lose the ended notice.** The same window between `branch/ended` and the roster input drops the ended notice a line's outside end would have written. The load heal writes the roster alone, so `main` learns it from the roster's retirement rather than from a notice.
- **The roster is short after compaction.** After `main` compacts, the roster keeps only each line's id and the first line of its purpose. What `main` told a line survives only in the compaction summary or through `read`, and `read` brings that line's raw events into main's context, one page at a time.

## 7. Identity and context

### 7.1 One key, `branch:line`

Every line answers "which line am I, and which lines are mine" with blocks under one key, `branch:line`. The key is the same on every line.

- **On a child: its identity.** TimelineTree writes it on every child, tool line or host line. It is the first block of the child's opening Queue input, keyed `branch:line`, and it states:
  - the child's id, its parent and the cut;
  - that the child is the same agent and the history above is its own;
  - that it works in parallel from here on;
  - that messages above addressed to the parent, such as the roster, belong to the parent.

  A tool line's identity also carries its purpose (the `create` prompt) and the line protocol. On a host line, the creator's prompt stays the opening input's `user` content, after the block.
- **On `main`: the roster.** BranchLine on `main` writes a keyed Append from `main`'s own facts:
  - `branch/create` with a tool source;
  - `branch/create-failed`;
  - `branch/ended`.

  The roster lists each open tool line with its id and the first line of its purpose. When no tool line is open any more, the key is retired.
- **Superseding.** A child inherits its parent's roster in its prefix. Its own identity block, under the same key, supersedes that roster. From the child's opening input on, the latest live `branch:line` block on the child is its identity.

Key and mode are independent ([history.rs](../../crates/dim-agent-capabilities/src/history.rs) `render_input`). Goal's `set` already puts keyed blocks in a Queue input ([Goal §5](plugins/Goal.md#5-context-mapping)).

The identity is part of the opening input, not an Append written at `branch/start`, for two reasons:

- An Append can commit after the opening input ([Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal)).
- The purpose is not known at `branch/start`.

### 7.2 Compaction

Compaction is per line. A compaction is a `context/compaction` fact on the compacting line's own Timeline.

- A child inherits its ancestors' compaction records up to its cut. Event ids are stable across the inherited prefix, so `retain_from` replays the same way on the child ([ContextCompaction](plugins/ContextCompaction.md)). After the cut, the child compacts on its own.
- A child's compaction never changes its parent, and a parent's compaction never changes a child.

The keyed baseline replays the latest live block of each key from before `retain_from`. As a result:

- when `main` compacts, its roster replays;
- when a child compacts, its identity replays.

One key means every line's baseline holds exactly one answer to "which line am I". An inherited answer can never outlive the line's own. Messages carry no key: after a compaction they survive in the summary or in the retained turns, like any other input.

### 7.3 Cache prefix

Lines differ only in their tails:

- The root adopts system and model. Children do not change them ([prompt](../architecture/rust-agent/prompt.md)).
- Prompt sources load with main's scope and are inherited ([PromptSources](../architecture/rust-agent/plugins/PromptSources.md)).
- One ToolRegistry per Session gives every line the same tool list. That is why `create` on a child is refused through the tool result instead of by removing the tool.
- The identity block is the child's first event of its own, directly after the inherited prefix. A child's first request therefore reuses the parent's prefix up to the cut.
- Roster updates are Appends at the end of `main`'s history.

### 7.4 Goal and Pursuit

**Rule.** A line's opening input is TimelineTree's write. Its first block is the `branch:line` marker, so under [Goal A5](plugins/Goal.md#appendix-e-assumptions) (operator input is "`user` content and no source marker as first raw block") it is a sourced input, not operator input, whatever `user` content the creator gave. Goal's conflict table records this ([Goal Appendix B](plugins/Goal.md#appendix-b-full-conflict-table)). Goal's own rules are unchanged. Its code changes only through the [Pursuit amendment](plugins/Pursuit.md#13-ending-the-evaluator-with-427).

Before this design, a host line's opening input (the creator's prompt as `user`, no block) counted as operator input. A tool line's did not: it had the `dim-branch-task` block and no `user`. Both are now sourced inputs.

Why this changes no Goal behavior:

| Goal decision | Effect of the identity block |
| --- | --- |
| The block-based decisions: the decided set, the continuation list, `conclusionOf` and the keyed objective and status fold | None. `marker()` matches only `^<dim-goal `, and the fold also requires the `goal:*` keys, so the identity block is invisible to all of them |
| The operator test | This is the one field it changes: the opening input no longer sets `lastOperator` |
| `count` | `lastOperator` is used only here: `since = max(active, lastOperator)` |
| The count window | The fold is local and starts over at the line's own opening. A child starts with no goal, and Goal writes the start `clear` ([Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal)). `active` moves only on a later local `set` or `resume`, and both always come after the opening input. So the opening input never changes `since`, and the count stays the same |
| Evaluator recognition | After the [Pursuit amendment](plugins/Pursuit.md#13-ending-the-evaluator-with-427), Goal recognizes the evaluator from `TimelineInfo.source` and `parent`, not from the first Queue input's `user` text. The identity goes in `blocks`, and `render_input` renders raw blocks before the operator's words ([history.rs:181](../../crates/dim-agent-capabilities/src/history.rs)), so the model sees the identity before the prompt. The A5 rule above stays as written |

## 8. Workspace

Lines share the Session's workspace. No line can know who changed a file, and the design does not try to attribute changes. The guard that already exists is enough:

- FileReads keeps a read binding per line.
- `edit` and `write` refuse with `file_modified_since_read` when a file changed after this line last read it, whoever changed it ([edit.rs](../../crates/dim-agent-capabilities/src/workspace_tools/files/edit.rs), [write.rs](../../crates/dim-agent-capabilities/src/workspace_tools/files/write.rs)).

The rest is protocol, carried by the tool line's identity and by the tool description:

- `main` gives each line its own files or area in the `create` prompt.
- A line tells `main` before it changes any file outside its area.
- On `file_modified_since_read`, the line reads the file again and tells `main` before it continues.

WorkspaceTools is unchanged.

## 9. The user

- The interaction surface belongs to the Session. Every line deals with the user as this Session.
- **Questions.** Any line can ask with `ask_user`. The answer returns as that call's tool result on the line that asked ([Questions](plugins/Questions.md)).
- **Approvals.** Approvals for a line's tool calls go straight to the user. Approval notices carry `branchId`, and clients label the line.
- **Nothing is relayed through `main`.** It is the same subject asking its own user.
- **Submitting.** The user submits to any open line (`session.submit` with `branchId`, unchanged).
- **Interrupting.** Interrupting a line ends its turn, not the line.
- **Ending.** The user ends any line except `main` (`session.branch_end`, §10).

## 10. Ending a line

| Event | Timeline | Data | Role |
| --- | --- | --- | --- |
| `branch/end` | The line that ends | `{source, ancestor?}` | Command |
| `branch/ended` | Its parent | `{branch, endEvent, source, ancestor?}` | Result, written by TimelineTree |

This pair mirrors `branch/create` and `branch/create-failed`: a command on one Timeline, routed by TreeTimeline, and a result from the tree. `endEvent` is the sequence of `branch/end` on the ending line.

`source` is the existing `BranchSource`, the same type `branch/create` uses, so a create and an end record their issuer the same way:

| Source | Issuer |
| --- | --- |
| `{kind: "tool", turn_id, request_id, call_id}` | A tool call on the ending line itself: the `branch` tool's `end` |
| `{kind: "host", operation_id}` | A host operation. Either App Server's `session.branch_end`, which mints `operation_id` the way `session.branch_create` does (a uuid, [connection/tree.rs](../../crates/dim-app-server/src/connection/tree.rs)), or a plugin with its own id, such as Goal's `goal:evaluate:N` |

`ancestor` names the line whose own end started the cascade: the end that carries no `ancestor` of its own. Every descendant the cascade ends carries that line, `source` is copied from that same end, and the pair therefore names one end operation: who asked, and on which line. A line ended at its own request has a `source` and no `ancestor`, and a descendant whose own end was already committed keeps that pair when the cascade reaches it. `branch/ended` copies `source` and `ancestor` from the line's `branch/end`, and is always written on the ending line's own parent, taken from the directory.

**A general operation.** `TreeCommands.end(id, source)` and `Session::end_branch(id, source)` check only the target: it exists, it is open, and it is not `main`. Ending `main` would close the Session, so that check is about the target, not the caller. They never check who calls.

- Ending a line that has already ended writes nothing, and the reply is `branch_ended`.
- A line still opening is not a line yet: its creation can still fail, and one terminal fact per line is the rule, so `end` answers `unknown_branch` until the line is published. An opener that ends at load waits for the Session, which publishes after its restore (§17 R1).
- Closing the Session is not an end.
- "The opener manages its line" is a convention among callers, not a runtime rule:
  - the `branch` tool's `end` takes no target, so it ends the calling line, by the tool's design;
  - a parent that wants a child to end asks it with a steer, which is the tool's convention.

Sequence:

1. The entry commits `branch/end` on the line. From then on the line accepts no new input and opens no line, and no line opens under any line below it: those entries fail with `branch_ended`. A line below takes input until its own `branch/end` (step 3).
2. TreeTimeline on the line consumes `branch/end` and hands it to TimelineTree.
3. TimelineTree ends the line's descendants first, through these same steps, each with `source` copied and `ancestor` naming this line, however deep it is.
4. The close happens:
   - for a `tool` source: at the line's first turn end after `branch/end`, because the call is part of that turn, or at once if the line is idle;
   - for a `host` source, and for a line ending because of an ancestor: at once, interrupting the current turn.
5. `Branch::close()` interrupts the line, settles its work and unloads it ([branch.rs](../../crates/dim-agent-runtime/src/branch.rs)).
6. TimelineTree writes `branch/ended` on the parent, last.

On the parent, BranchLine acts only for tool lines; host lines belong to their creators:

- When a tool line ended itself through the tool (a `tool` source and no `ancestor`), it writes the roster update only. The line's final message already reached `main` as a steer before `branch/end`.
- For any other end of a tool line, it writes the roster update plus the ended notice (§12.1), as a steer, in the same input.

Clients show "ended itself" for a `tool` source and no `ancestor`, and otherwise the operation that ended the line; a line ended by a cascade names its ancestor, for example "ended with X".

**Restore.** A child is inactive when its parent has a `branch/ended` for it. The resumed parent's history read reports these (§5). This joins the existing rules: the parent is inactive, the parent has a `branch/create-failed` for the child, or the child's creation never completed.

A line that has `branch/end` but whose parent has no `branch/ended` for it is not opened. Restore completes such ends deepest line first, writing each missing `branch/ended` on the parent with `source` and `ancestor` taken from `branch/end`, and a line's own result last. For a line below it, that parent is not opened either, so the record joins its stored history and its fixed head follows. A line below one of them whose own end never started gets no `branch/ended`: the parent is closed and never replays, so nothing would read it. Restore records each such line as ended at the head it stopped at, with the `source` its parent's `branch/create` gave it, and its history stays readable (§1, "After the end").

**After the end:**

- The line stays in the branch directory, and its history stays readable from the store, through `session.history` (v3) and through the tool's `read`. Its head never moves.
- A fork copies directory rows and main's events as it does today. A line that ended before the fork point is inactive in the fork too, because its `branch/ended` is in the copied prefix.
- Deleting the Session removes the line along with it.
- Session close and idle unload are not ends: lines restore as open.

**Plugin lines.** Each opener ends its own lines under its own protocol. Goal ends its Pursuit evaluator once the evaluation is settled, and treats an outside end before the conclusion as `failed` ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)).

## 11. The `branch` tool

The tool takes one action-based input, which replaces `{prompt, at?}`. Its risk stays `session`, and Approval admits each call as it does today.

| Action | Input | Who | Effect | Result text |
| --- | --- | --- | --- | --- |
| `create` | `prompt`, `at?` | `main` | Opens a tool line cut at `at` (default: before this request's model start, as today) | `Line created: C` |
| `message` | `to`, `content`, `mode?` (`append` \| `steer`) | `main` → its open tool line; tool line → its parent | One `input/message` on `to` | `Sent to C.` |
| `read` | `branch`, `after?`, `limit?` (1–200) | `main`, on its tool lines | One page from the store | The page as JSON: items, `nextCursor`, `done` |
| `end` | `content` | A tool line, on itself | A final message (steer) to the parent, then `TreeCommands.end(C, source)`, where `source` is this call | `Line C is ending; your final message was sent to P.` |

The input schema is `{action, prompt?, at?, to?, content?, mode?, branch?, after?, limit?}`, with `action` required and `additionalProperties: false`.

Refusals come back as error results (code in `structuredContent`) and write nothing:

| Case | Text | Code |
| --- | --- | --- |
| `create` off `main` | `Only main opens lines. Tell main if more parallel work is needed.` | `branch_not_main` |
| `message` or `read` outside kinship | `You can message only the lines you opened and the line that opened you.` | `branch_not_kin` |
| `message` or `read` on a host line | `Line C was not opened with this tool; it follows its own protocol.` | `branch_not_tool_line` |
| `message` to an ended line, or a race with an end | `Line C has ended. Read it to see what it did.` | `branch_ended` |
| `end` on `main` or a host line | `Only a line opened with create can end itself.` | `branch_cannot_end` |

These refusals are the tool's own rules. `TreeCommands.end` itself checks only the target (§10). An `end` whose line already committed its own `branch/end` takes the `branch_ended` row and writes no second final message: the first call's message is the line's last word.

## 12. Prompts

### 12.1 Context mapping

`C` is the line, and `P` is its parent (`main` for a tool line). Content inside markers is escaped as TimelineTree escapes it today (`context_block`).

| Write | Where, key | Mode | Text |
| --- | --- | --- | --- |
| Identity, tool line | `C`, opening input, first block, `branch:line` | Queue | `<dim-branch-line branch="C" parent="P" source="tool">` / `You are line C of this session. Line P opened you at this point of its history to work in parallel with it. You are the same agent as P: everything above is your own history, and you continue from here on your own line. P keeps working on its line; you do not see its work after this point, and it sees yours only through what you send it. Messages above that were addressed to P, such as its list of open lines, are P's, not yours.` / blank / `Your purpose, as P wrote it:` / `<prompt>` / blank / `How you work with P:` / `- Work on this purpose only. You cannot open lines; if more parallel work is needed, tell P.` / `- Tell P with the branch tool (message, to "P"). Use mode "steer" for what P must act on now: a finding that changes its work or another line's, a blocking question, a conflict. Use "append" for progress P should see at its next step. Do not repeat what P already knows.` / `- You share the workspace with P and its other lines. Change only the files P gave you; before changing any other file, tell P which. If edit or write fails with file_modified_since_read, another line may have changed the file: read it again and tell P before you continue.` / `- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.` / `- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. That is your last action; P receives it.` / `</dim-branch-line>` |
| Identity, host line | `C`, opening input, first block, `branch:line`. The creator's prompt is the input's `user` | Queue | `<dim-branch-line branch="C" parent="P" source="host">` / `You are line C of this session, opened from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You do not see P's work after this point. Messages above that were addressed to P are P's, not yours. The message after this one says what this line is for.` / `</dim-branch-line>` |
| Roster | `P`, `branch:line` | Append | `<dim-branch-lines branch="P">` / `Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it does not see your work after that point unless you message it.` / `- C1: <first line of its purpose, at most 120 characters>` / … / `They message you when they find something, need you, or end. Use the branch tool to message or read them, and wait to wait for them.` / `</dim-branch-lines>` |
| Roster, none open | `P`, `branch:line` retired | Append | `<dim-branch-lines branch="P">` / `All lines you opened have ended.` / `</dim-branch-lines>` |
| Message | Receiver, no key | `append` or `steer` | `<dim-branch-message from="C" to="P">` / `<content>` / `</dim-branch-message>` |
| Final message | `P`, no key | Steer | `<dim-branch-message from="C" to="P" final="true">` / `<content>` / blank / `Line C ends with this message.` / `</dim-branch-message>` |
| Idle notice | `P`, no key | Steer | `<dim-branch-idle branch="C" status="completed\|failed\|cancelled">` / `Line C's turn ended (<status>) without a message to you, and C is waiting. Read it if you need its state; message it with mode "steer" to give it more work, or leave it.` / `</dim-branch-idle>` |
| Ended notice | `P`, no key, in the roster's input, for any end of a tool line other than its own `end` | Steer | `<dim-branch-ended branch="C">` / `Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did.` / `</dim-branch-ended>` |
| Tool description | Every line | — | `Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed while you work.` / `- create (main only): open a line to work on prompt in parallel. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Open a line only for work that can go on without you. Returns the line's id.` / `- message: send content to a line you opened, or to the line that opened you. Mode "append" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use "steer" when the receiver must act now or is idle.` / `- read: show the recent activity of a line you opened, also after it ended. Pass nextCursor as after to continue.` / `- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.` / `Lines talk only with the line that opened them and the lines they opened.` |

These texts replace the delegation wording in the code today: the `dim-branch-task` block in `first_input` ("That branch delegated this task to you") and the tool description ("works on prompt as a task you delegate").

### 12.2 Items for the prompt layer ([#429](https://github.com/arcships/dim/issues/429))

The texts above are first versions, owned by TimelineTree's plugin. These items belong to the model-facing prompt layer:

- When `main` should open a line: the work is parallel and separable, and it can go on without `main`. When `main` should not: the work is a short step, or it needs `main`'s next decision.
- How `main` briefs a line, and what coordinating means for `main`: assigning areas, answering steers, ending its own wait, and not redoing a line's work.
- **Goal's wording (owned by Goal).** These texts treat a child as delegated work, which contradicts §1:
  - the objective text, "If you are working on a delegated sub-task started from branch X, treat this goal as background and do not call goal_update." (`runtime/plugins/goal/goal.ts:98`);
  - the continuation text (`goal.ts:131`);
  - "delegated work" in [Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal) and in Goal §5's row for child branches.

  The replacement is "on another line of this Session". This design does not change that text; Goal owns it.

### 12.3 Evaluation scenarios

These are run locally on real models, and the results are reported in the implementation PR. CI asserts only behavior.

| Scenario | Watch for |
| --- | --- |
| A line acts as main | It opens lines, claims the roster, or coordinates other lines |
| Redone work | `main` repeats what a line is doing, or two lines do the same thing |
| Stale knowledge | A line acts on files or decisions that changed after its cut without re-reading them |
| Flooding | Append or steer messages that repeat what the receiver already knows |
| Over-splitting | Lines opened for short or sequential steps |
| Never ending | A line that is done but does not call `end` |
| Sibling relay | `main` is used to pass messages between lines that should have been one line |
| Identity after compaction | After compaction on `main` and on a line, each still knows which line it is and which lines are its own |
| Ping-pong | Steer exchanges that do not move the work forward |
| Asking the user | A line asks the user directly when it should, and does not route through `main` |

## 13. Interactions

| Feature | Interaction |
| --- | --- |
| Goal | Goal's own rules are unchanged. A line's opening input is a sourced input under A5 (§7.4), and Goal's conflict table records it. A new line clears the inherited goal as before. Goal's code changes only through the Pursuit amendment: recognition from `info(own id).source`, ending its evaluator, and the `branch/ended` key ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) |
| Pursuit | The evaluator gets the host-line identity block. Goal recognizes its evaluator from the line's source and ends it once the evaluation is settled ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) |
| Fork, delete | Unchanged. Ended lines follow their facts (§10) |
| Session close, idle unload | Not an end. Lines restore as open |
| Interrupt | A turn interrupt, or a stop on a line, ends a turn and never a line. An interrupt on `main` does not touch its lines |
| Collaboration | Unchanged. Members are Sessions; whether a member's Session mounts `branch` is up to its profile's tool list |

## 14. Clients

**TUI.** Lines get their own panel, view and controls:

- `/lines` (alias `/branches`) lists every line of the Session, each indented under the line that opened it, with its state: working, waiting, ended itself, ended by an operation, ended with X, or creation failed.
- Enter shows the selected line's conversation. The composer submits to the line shown, and the composer's bottom border names it, with the first eight characters of its id.
- `x`, then `y`, ends the selected line, refused on `main`.
- Questions and approvals carry the line's label, so two lines asking for the same thing are told apart (`(branch NAME)` on a question, `Permission on branch NAME` on an approval).
- A message between `main` and a line reads as sent on the sender (its `branch` tool call) and as received on the receiver (`◂ from C`).
- Switching the shown line starts another transcript under a boundary row (`line C`), as a session switch does, because terminal scrollback is append-only. The line's own transcript folds from its events while it is shown or not, so opening a line reads no history.

The panel is separate from the tasks panel, which lists background commands, members and Workflow runs: its `x` stops a task, and an end is permanent, so the two do not share keys.

```
┌ session 3f2a · main ────────────────────────────────────────────┐
│ … main's conversation …                                         │
│ ◂ from b-7c (steer)  the leak is in cache.rs                    │
│ ◂ from b-02 (steer), final  docs pass done                      │
│                                                                 │
│ ╭─────────────────────────────────────────────────────────────╮ │
│ │ › ask anything…                                             │ │
│ ╰───────────── ~/repo/dim (main) ────────────────────────────╯ │
└─────────────────────────────────────────────────────────────────┘

┌ lines ──────────────────────────────────────────────────────────┐
│ ● main                          the session itself · shown      │
│   b-7c  parser race hypothesis          working                 │
│   b-91  review the cache change         waiting                 │
│     b-c3  the guard in cache.rs         working                 │
│   b-02  docs pass                       ended itself            │
└────────── Enter show the line · x end it · Esc close ───────────┘
```

**Web** shows `main` only today, and lines come later. Desktop gets the protocol: `session.branch_end` and the two events in v3 history.

## 15. Changes

| Module | Change | Why |
| --- | --- | --- |
| [Design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义) | Rewritten: lines of one subject, shared facts, parent and child as the only communication relation, and the shared interaction surface. Relations row: "only between parent and child". Session row: adds "end" | The current text, "equal lines, the relation only records the source", has no communication relation |
| TimelineTree | The identity block in `first_input` for every source. `TreeCommands.end(id, source)` and `TreeCommands.page(id, after, limit)`. `TimelineInfo.source`, filled from the parent's `branch/create` (§5). The end flow, refusing input once a line is ending, `branch/ended`, the restore rule, and reads of ended lines | It owns the line lifecycle and the opening input |
| TreeTimeline | Consumes `branch/end`, plus the line's next turn end for an end issued by a tool call, and routes them to TimelineTree | It routes `branch/create` the same way |
| BranchTool | The action-based tool, plus the new BranchLine branch part | The tool's actions and one line's view of its lines |
| Session facade | `Session::end_branch(id, source)`. History reads of an ended line (`local_page`, `sequence`) from the store, at its fixed head | §17 |
| App Server | `session.branch_end` (v3, a host source with a minted `operation_id`, listed in `hello.features` with the other Session methods when `session_execution` is on and the connection's entry is not bare or the daemon profile is not protected). `session.history` on an ended line returns immediately, at its stored head. `ProductEvent::new` carries the two kinds in v3 and leaves them out of v1 and v2 | An entry and a read, no business logic |
| dim-agent-types | `branch/end` and `branch/ended` in `EventKind` and the v1 exclusion list, registered in the contract tests. The frozen fixtures are unchanged | Two core facts (§16) |
| Goal (Pursuit amendment) | Recognizes its evaluator from the line's source and ends it ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) | Goal owns its evaluator's protocol |
| Branch TypeScript binding | Regenerated `npm/branch-services.d.ts`: TreeCommands gains `end` and `page`, and `TimelineInfo` gains `source`. The `treeCommands` row of the [binding table](branch-typescript.md) lists `end` | TreeCommands is an exported service |
| CLI TUI | §14 | Users follow, talk to and end lines |
| Plugin pages | TimelineTree.md and BranchTool.md, updated with the implementation (`xtask check`) | Pages follow the code |

New public content:

- two events;
- one RPC method;
- two TreeCommands methods: `end(id, source)` and `page(id, after, limit)`;
- `TimelineInfo.source`, read by BranchLine and by Goal;
- the `branch` tool's new input.

BranchLine's service is read only by the tool, inside the same plugin.

## 16. Format

`branch/end` and `branch/ended` are core events in the `branch` domain:

| Event | Data |
| --- | --- |
| `branch/end` | `{source, ancestor?}`. `source` is a `BranchSource` (`{kind: "tool", turn_id, request_id, call_id}` or `{kind: "host", operation_id}`); `ancestor` is a line id |
| `branch/ended` | `{branch, endEvent, source, ancestor?}`. `endEvent` is the sequence of `branch/end` on the ended line; `source` and `ancestor` are copied from it |

The format decision for them is recorded in the storage contract: [#427 v20 decision](appserver/native-storage-format.md#427-v20-格式决定).

## 17. Requirements raised to mechanism owners

**R1. Durable end of one line** (owner: dim runtime, TimelineTree and the Session facade; implemented with this design under #427).

- **Blocked feature and rule.**
  - A line cannot end on its own. `Branch::close()` exists, but no entry calls it and the close is not recorded, so restore opens the line again.
  - The Session facade reads history only from published lines, so an ended line's history becomes unreadable.
  - The facades and the event format are infrastructure ([design rules §4](../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 8).
- **Existing mechanisms considered.**
  - `stop/interrupt` ends a turn, not a line.
  - Session close ends every line.
  - `branch/create-failed` exists only before a creation completes.
  - A plugin-owned event cannot work, because restore belongs to TimelineTree and cannot read plugin facts (rules 4 and 7).
  - Reading the store directly from App Server would bypass the facade.
- **Generic capability.** It carries no business meaning:
  - a command `branch/end {source, ancestor?}` on the line and a result `branch/ended {branch, endEvent, source, ancestor?}` on its parent;
  - `TreeCommands.end(id, source)` and `Session::end_branch(id, source)`, which check only the target;
  - `TimelineInfo.source`, filled from the parent's `branch/create`;
  - no new input once `branch/end` is committed;
  - descendants end first;
  - restore treats an ended line as inactive and completes a `branch/end` that has no `branch/ended`;
  - an ended line's committed history stays readable at its fixed head, through `Session::local_page` and `TreeCommands.page`.
- **Effect on sequences, batches, interrupt and reload.**
  - Each event is its own batch: `branch/end` on the line, and `branch/ended` on the parent after the close.
  - An end with a `host` source, or because of an ancestor, interrupts the line through `Branch::close()`.
  - On reload, the restore rule above applies. A Session publishes after its restore, so a part that ends a line at load, such as Goal's reconcile ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)), waits for `SessionLifecycle::activated()` and then finds every line the directory holds published.
  - The Timeline gains the line's own admission check: `Timeline::input` becomes `input_checked(data, admit)`, which runs `admit` under the commit lock, so an input that commits after `branch/end` is refused with `branch_ended` instead of racing the close. The Dispatcher and batching are unchanged.

## 18. Security

- Lines share the Session's permission policy and Approval. A line is not isolation ([ADR 003](../adr/003-approval-scope.md)).
- Only TimelineTree and BranchLine write the markers. Other producers that write blocks (plugins, through `TreeCommands.input`) are trusted in-process parts. RPC writes no blocks.
- `session.branch_end` is a trusted user entry, as `session.branch_create` is. It needs no approval, because it is not an agent action.
- Any trusted in-process part can end any open line except `main`, just as it can write input to any line. Approval governs the tool's `end` like any other call.
- Diagnostics record ids, sequences and the source kind, never message or purpose text.

## 19. Rule check

Rules: [design rules §1, §2 and §4](../architecture/rust-agent/principles.md), [ADR 002](../adr/002-plugin-scope.md) D1–D5, and [ADR 003](../adr/003-approval-scope.md).

| Rule | How this design complies |
| --- | --- |
| §1 Capabilities, App Server boundary | Capabilities: TreeTimeline (existing) and BranchLine (new). TimelineTree and BranchTool's Session part are ordinary parts. App Server gains one entry and one read path, with no business code |
| §2 Scopes | The line lifecycle stays the Session's (TimelineTree). One line's view of its lines is branch state (BranchLine). Lines influence each other only by writing input, now only between parent and child (§2 rewritten). Capabilities affecting a branch are not bound by that rule (§4) |
| §4.1 Command events | An end is `branch/end`. TimelineTree decides and writes `branch/ended`. A message is input whose receiver's InputQueue and Loop decide. An end of an ended line, or a refused message, writes nothing, and the reply says why |
| §4.2 No recall after handoff | A committed message belongs to the receiver. A `branch/end` is not withdrawn |
| §4.3 Before callbacks | None |
| §4.4 No business meaning in core | Dispatcher, `before` and batches are unchanged; the Timeline's admission check is generic (§17 R1). The identity text is TimelineTree's own domain, the lines of a Session. A tool line's protocol is written where `first_input` already writes tool-specific text, in the module that owns the tool |
| §4.5 Batches | `branch/end` stands alone. `branch/ended` comes last, after the close. The roster and the ended notice are one input |
| §4.6 Context | Identity, roster, messages and notices are adopted inputs. `read` returns a tool result. Nothing else enters the context |
| §4.7 Public events and services | BranchLine folds its own line's public events. The tool reads BranchLine (same plugin) and TreeCommands. `read` returns committed events as a tool result, the way Collaboration's `read` does, and no part folds another line's facts. Goal classifies the opening input by its own published A5 convention (§7.4). BranchLine and Goal read `TimelineInfo.source` through TreeCommands, a public service |
| §4.8 Infrastructure | The facade and format changes are R1 (§17). They come from the line lifecycle itself and close within the runtime |
| §4.9 Only facts later decisions use | `branch/end` decides the close and the restore. `branch/ended` decides restore inactivity and the roster. No UI-only event |
| ADR 002 D1 Scope | As in §5 |
| D2 No wiring modules | BranchTool's feature registers BranchLine itself |
| D3 Scope dependencies | BranchLine depends outward on the Session's TreeCommands. No core part depends on it |
| D4 Configuration | None |
| D5 Language-neutral contracts | Events, the RPC method, markers and the tool schema are data. TreeCommands' new methods are exported and typed for TypeScript |
| ADR 003 Approval | The tool keeps risk `session`, and Approval admits every call. A user end is not an agent action |

## 20. Acceptance

These are flows with a fake model, in `branch_flow.rs`, `flows.rs` and the App Server transport flows.

1. `main` creates two lines. The roster lists both. Each child's identity supersedes the roster it inherited. After a compaction on `main` and on a child, the keyed baseline replays the roster or the identity.
2. A child sends `main` an Append: it does not wake `main`, and `main` sees it at its next request. A Steer wakes `main`'s `wait`, or starts a turn when `main` is idle. A Steer from `main` wakes an idle child.
3. These are refused and write nothing: a sibling message, `create` on a child, `message` or `read` on a host line, a tool line's `read` of `main`, and `end` on `main`.
4. When a child's turn ends without a steer, one idle notice reaches `main`, with the turn's status. None is written when the turn steered `main`, called `end`, or left a driving input pending.
5. A tool line ends itself: the final message (steer) and `branch/end` with a `tool` source are written, the close comes at the turn end, then `branch/ended` on `main` and the roster update, with no ended notice. A later message is refused (`branch_ended`). The tool's `read` and `session.history` return the ended line's history.
6. User end through `session.branch_end`: `branch/end` has a `host` source with App Server's id, the line's turn is interrupted, and `main` gets the ended notice as a steer. `main` cannot be ended. A second end returns `branch_ended`. Submitting to an ended line returns `branch_ended`.
7. Restore: an ended line is not opened, and neither are its descendants. A `branch/end` without a `branch/ended` gets its result written at restart.
8. A Pursuit evaluator gets the host-line identity, and the existing Pursuit flows pass unchanged.
9. Two lines edit the same file: the second gets `file_modified_since_read` (existing behavior).
10. v1 and v2 clients do not see the new kinds. v3 history includes them, and `hello.features` lists `session.branch_end`.

Goal flows, in `goal.test.mjs` and `transport_flow/goal.rs` (§7.4):

11. A line opened with `session.branch_create`, whose opening input carries the identity block, behaves exactly as `main` does: `set` on that line, its continuations, and a later operator input that resets the count.
12. A Pursuit evaluator created under the new opening input is recognized and its conclusion is applied. The working line's count is unchanged.
13. The identity block changes nothing in the objective and status fold, the decided set or the continuation list.

Line end and sources:

14. `TreeCommands.end` ends any open line except `main`, whoever calls it, for example a plugin part with a `host` source on a tool line, which gives `main` the ended notice. It refuses `main`, an unknown line and an ended line, and writes nothing.
15. Ending a host line that has a child ends the child first, with `ancestor` set and `source` copied, then the line.
16. `TreeCommands.list` and `info` return `source` for a new child and for a restored one, and none for `main`. The stored directory rows are byte-for-byte unchanged.

## 21. Ablation review

What is left, and why each piece is needed:

| Piece | Why |
| --- | --- |
| The identity block on every child, under one key | Without it a line forgets which line it is after compaction, or takes its parent's roster as its own |
| The roster on `main` | `main` must still know its open lines after compaction |
| Messages as `input/message`, with append and steer | The only path between lines. The modes are InputQueue's |
| `read` for `main` | `main` checks a line's work without asking for it |
| The idle notice | Without it `main` waits on a line that stopped silently |
| `branch/end` and `branch/ended` | Without them a line cannot end durably, and restore reopens it |
| `source` on the end (`BranchSource`) | A create and an end record their issuer the same way, so a plugin's end is not recorded as the line's own |
| `TimelineInfo.source` | An opener finds its own lines without parsing prompt text |
| The tool restricted to `main` and to tool lines | One coordinator, and creators keep their own protocols |
| The prompt rules on files | The only guard is FileReads; the rest is protocol |

Removed:

- caps on lines, messages, steers or rounds;
- attribution of file changes;
- changes to WorkspaceTools;
- a channel between siblings;
- per-line configuration (system, model, tools);
- a result contract and hand-back;
- a line reading `main`;
- separate events or concepts for messages (they are InputQueue modes);
- a parent ending its children through the tool;
- a `by` field with fixed values (the existing `BranchSource` records the issuer);
- a category field on host lines (`operation_id` already is the opener's tag);
- a runtime check of who ends a line.

## 22. Later options

**Per-line narrowing within the Session's limits.** Narrowing within the Session's limits keeps the same subject, so it is compatible with design rules §2. It is not part of this design. When it is built:

- **Fewer tools:** refusal at call time, as `create` on a child is refused now. The tool definitions and the cache prefix stay identical.
- **Lower permission:** a per-line policy intersected with the Session's. It needs the per-branch Permission narrowing that [Pursuit §10](plugins/Pursuit.md#10-later-options) already names.
- **Cheaper model:** per-line configuration set at create, before the first input. A different model shares no cache with its parent, so it trades the inherited prefix for cost.
