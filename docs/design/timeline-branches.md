# Timeline branches

The [target Session tree binding](contracts/runtime-owner-operations.md) moves calls from `treeCommands` to `timelineTree` with the same arguments and replies. The catalog entry and consumer injections change together. References to `treeCommands` below describe the pre-migration binding.

Status: Accepted ([#427](https://github.com/arcships/dim/issues/427)). This document holds both the requirements and the design: §1–§3 answer #427's questions, and §4 onward is the design. Scope: the TimelineTree plugin (TimelineTree, TreeTimeline, BranchTool, plus a new branch part for BranchLine), one App Server built-in method, one core event, one `BranchSource` kind, the CLI TUI, the Web workspace, and a rewrite of [design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义). Language: Rust. Chinese: [中文](../zh/design/timeline-branches.md).

This design delivers two capabilities: the Session's **default branch**, the line its conversation is on and choosing another open line as that line; and **who may open a line**, which is now the root always, plus whichever line is the current default branch. Every other line is refused, so at most two lines can open at any moment, and the root keeps the ability it has today.

One decision it does not take is the guidance for *when* a model should open a line: the four conditions and their boundaries with neighbouring mechanisms are stated in §12.4 as policy, and the change that first puts a candidate for them in place judges it with the scenarios in §12.3. A Session-wide cap on open tool lines sits beside the right to open (§6.1).

The English words are **default branch** for the role and **root** for the line with no parent, whose id is always `main`. The UI keeps 主线 for the default branch and 起点 for the root.

## 1. What branches are for

A **branch** is a line of the same agent inside one Session. The Session is one subject, the "I": its identity, its interaction surface with the user, its capabilities and its workspace. A branch is one line of that subject's behavior.

A child starts from its parent's history at a cut. Every event up to the cut is the child's own history as well, and those rows never change. After the cut, the child reads its own events, and through the `branch` tool it reads the line it was cut from (§6).

Lines share facts, not context:

- Each line assembles its own context from its own facts.
- Each line compacts on its own.
- A line influences another line by writing input to that line's Timeline, and it learns about another line from its own reads of that line.
- A rule or state change an ancestor appended without its delivery input consumed in the inherited prefix is not part of the line's inherited state: the line adopts the source's current value itself, and that block is its own event after the cut ([Prompt](../architecture/rust-agent/prompt.md)).

The parent–child relation records where a history came from. It is also the only relation along which lines talk and read. It does not mean control, and it does not mean that a result is owed.

Answers to #427:

| # | Question | Answer |
| --- | --- | --- |
| 1 | Which scenarios, in what order | Three, in order (§3): plugin lines (delivered), lines the agent opens from the root (delivered), and the user choosing an open line as the Session's default branch (this design) |
| 2 | Branch versus Collaboration member versus fork | §2. A branch is the same agent. A member or a fork is another Session |
| 3 | When a branch finishes, who needs its result, and in what form | No line owes another a result. Lines tell each other what matters while they work (§6). A tool line ends with a final message to the line that opened it (§10). A plugin line follows its creator's protocol, for example Pursuit's conclusion |
| 4 | When is a branch over, and is it visible afterwards | When it ends itself, the user ends it, or an ancestor's end cascades to it (§10). An ended line stays in the branch directory. Its history stays readable to clients and to the line that opened it |
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
| S1 | **Plugin lines.** A plugin opens a line under its own design. Example: [Pursuit](plugins/Pursuit.md)'s evaluator, a host-sourced child cut at the worker's turn end, whose conclusion goes back as input (implemented, [#422](https://github.com/arcships/dim/issues/422); `crates/dim-goal`) | Delivered. This design adds the identity block (§7.1). Goal's recognition and end of the evaluator change with it ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) |
| S2 | **The agent opens lines from the root.** Examples: test two hypotheses for a bug on two lines while the root keeps reading; implement on the root while a line reviews each change as it lands; research an API or one area of the code while the root continues; try a second approach on separate files | Delivered. This design neither changes who may open a line nor adds guidance for when to open one |
| S3 | **The user chooses an open line as the Session's default branch and continues there.** `session.branch_create` already opens lines under any parent. With this design the user makes any open tool line, or any line they opened themselves, the line the Session's conversation is on, and moves back to the root with the same action. The line keeps the opener it was cut from, so coordination does not move. Compare and adopt need a design of their own: what adopting a line means for the shared workspace | This design |

These needs are not branches:

| Need | Why it is not a branch | Where it belongs |
| --- | --- | --- |
| Delegate to a specialist with its own instructions | It is another identity | Collaboration member |
| Judge whether to approve a tool call | It is a decision about one action, not a line of work | A capability with a host port ([ADR 003](../adr/003-approval-scope.md)) |
| Rewind a line to an earlier point | It changes how a line sees its own history; it is not a new line | A separate requirement. Today the answer is `session.fork` |
| Make edits in parallel, in isolation | Lines share the Session's workspace, and no per-line workspace exists | A fork, or a member with its own working directory |

## 4. Tree and roles

- **Depth.** The architecture allows lines at any depth. `session.branch_create` takes any parent. Goal accepts any `branchId`, so an evaluator opened under a child is a grandchild.
- **Kinship.** Parent and child is the only relation along which lines communicate. A parent and each of its children write input to each other, and a line reads the line it was cut from (§6). Siblings and more distant relatives do neither. The rule covers one line acting on another, which here means the `branch` tool. A capability that decides on its own is a capability affecting a branch, and this rule does not limit it ([design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义)). Examples are Goal writing an evaluation result, and a plugin opening a line at any cut.
- **The root.** `main` is the line with no parent and no source: the literal id, its numbering and every stored `branch_id: "main"` are unchanged by this design. It never ends, and the Session's close is not an end. It opens lines with the tool in every state, as one of the two callers the tool admits (§4, §11). In the UI it is 起点.
- **The default branch.** The line the Session's conversation is on: the target a request names when it omits `branchId`, and the line a client centres. It is a position in the tree, not an id and not a tree relation: it starts as the root and moves only by a user action (§9, §14). One authoritative non-model fact records it, `branch/default {branch}`, written on the root line, which outlives every other line and cannot end (§16). A keyed block that tells a line about the change is never the authority. A Session with no such fact is on its root.
- **The root and the opener come from the tree.** A line is the root when it has no parent. A line's opener is the parent recorded in its own `branch/create`. Who opened a line, who receives its reports and who coordinates it are that one edge, read from the tree: this design adds no role marker, no coordinator field and no stored relation for them.
- **An opener.** A line coordinates the lines it opened with the tool: it carries its own roster under `branch:roster` (§7.1), which lists only those lines, and it is the only line that messages and reads them. A line the user opened is on no roster, so the line it was cut from neither messages nor reads it (§6); it coordinates the tool lines it opens itself, from its own roster. An opener does not end its lines: `end` has no target and ends the caller, so an opener that wants a line ended asks it with a steer or the user ends it (§10, §11), apart from what a line's source forbids: a plugin line follows its creator's protocol, and a line the user opened is ended by the user. Coordination follows the parent-child relation and never moves with the default branch. A line that loses the right to open keeps the lines it already opened and keeps coordinating them.
- **The agent's tool.** The `branch` tool opens a line from two kinds of caller, and refuses every other one (§11):
  - **the root**, always, whether or not it is the default branch. Its right never changes, so it never receives a status note about the role (§12.1);
  - **the current default branch**, while it holds the role.

  A line that is neither is refused `branch_not_opener`, and the refusal says which lines can open one in this Session. A plugin line never opens a line. A former default branch keeps the tool lines it opened and coordinates them; it just cannot open another. At most two lines can open at any moment, and a Session with the root as its default branch has one opener.

  This is what keeps the root able to act on the user's own request: while a line is the default branch, the user can still ask the root for a line, and that line starts from the root's history and reports to the root. A guard that let only the default branch open a line would refuse that request, and the same request sent to the default branch would start from the default branch's history, which does not carry what the root did since the split.
- **Sources.** Every line has a source, recorded in its parent's `branch/create`. The source records where the line was created from; it is not a role marker, and this design reads nothing from it beyond what is stated here:
  - A **tool line** was opened by the `branch` tool. It is a conversation line: it can be the default branch, and it ends itself with `end` while it is not the default branch.
  - A **user line** was opened by a client through `session.branch_create`. Its opener is the user, and it is a conversation line too: it can be the default branch. The user ends it; it cannot end itself.
  - A **plugin line** was opened by a plugin (`TreeCommands.create`). Its creator owns its protocol, and it can never be the default branch. It opens no lines either.

  The source can be read as `TimelineInfo.source`, returned for active lines by `TreeCommands.list` and `info` and absent on `main` (§5). Ended directory entries may omit it; opening their display detail reads the creation source at its known position. A plugin line's source is `{kind: "host", operation_id}`: the opener's opaque tag, which the core never interprets. A user line's source is `{kind: "user", operation_id}`, its own kind, so a reader that must tell a conversation line from a plugin line does not parse an `operation_id`. An opener finds its own lines by its own operation-id convention, for example Goal's `goal:evaluate:` prefix. The kind says where the line was created from, and nothing about a role.
- **Lines created before the upgrade.** `session.branch_create` persists `{kind: "host", operation_id}` today ([connection/tree.rs](../../crates/dim-app-server/src/connection/tree.rs)), and this design rewrites no stored row. A line the user opened through a client before the upgrade, Web `/branch` under `FORMAT` 52 included, therefore stays `host` and is not a valid switch target afterwards, while the same action on a line opened after the upgrade works. This change accepts that limit instead of migrating the fact: `FORMAT` 53 is a marker-only bump that adds no data (§15, §16), and an upgrade case states it (§20).
- **Reach of the tool.** A line messages and reads the line it was cut from and each open tool line it opened. A line the user opened stands on both sides of that rule: it messages the line it was cut from and reads it from its cut, and it coordinates the tool lines it opens; the line it was cut from neither messages nor reads it, because the user opened it (§6). `end {content}` takes no target, so it ends the calling line and no other (§6, §10, §11). A plugin line is not on any roster, and the tool neither messages it nor ends it.
- **The parent block.** A line's BranchLine writes one keyed `<dim-branch-parent>` block on the line itself when the line it was cut from ends a turn and has new activity past the line's last read, the last head the line was told and the line's opening (§7.1.1, §12.1). The block reports positions and is state, not a request; the content stays in the pages a read returns.

| Line | Opens lines with the tool | Messages and reads, with the tool | Ended by, by convention |
| --- | --- | --- | --- |
| `main`, the root | Yes, whether or not it is the default branch | The tool lines it opened | Never; the Session's close is not an end |
| The default branch | Yes, while it holds the role | The line it was cut from and the open tool lines it opened | Nobody while it is the default branch, whoever asks; the user moves the default branch elsewhere first and then ends it |
| Any other tool line | No (the tool result refuses), but it keeps the tool lines it opened | Its parent and the tool lines it opened | Itself (`end`), the user, or an ancestor's end |
| Any other user line | No (the tool result refuses), but it keeps the tool lines it opened | The line it was cut from, which it messages and reads from its cut, and the tool lines it opened. The line it was cut from neither messages nor reads it (`branch_not_kin`) | The user or an ancestor's end; not itself |
| Plugin line | No | — (its creator's protocol) | Its creator (Goal ends its evaluator, [Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)), the user, or an ancestor's end |

Who ends which line is a convention among callers, apart from the lines the tool refuses to end on their own behalf: a plugin line follows its creator's protocol, and a user line is ended by the user (§11). The runtime checks only the target, whoever asks, the user included: it exists, it is open, and it is neither the root, the default branch, nor an ancestor of the default branch, because ending a line ends every line below it (§10).

## 5. Parts

Every part belongs to the TimelineTree plugin ([timeline_tree.rs](../../crates/dim-agent-capabilities/src/timeline_tree.rs)), and each sits in the scope that owns its state ([ADR 002](../adr/002-plugin-scope.md) D1).

| Part | Scope | Capability | Owns in this design |
| --- | --- | --- | --- |
| TimelineTree | Session | No | The line lifecycle: the identity block in every child's opening input, `TimelineInfo.source`, the end of a line (`branch/end` → `branch/ended`), refusing input once a line is ending, the restore rule, reads of an ended line's history, the default branch and its `branch/default` fact, and the branch directory (§14) |
| TreeTimeline | Branch | Yes | Routes `branch/end` to TimelineTree, as it routes `branch/create` today. For an end issued by a tool call, it also routes the turn end that the close waits for |
| BranchTool | Session | No | The action-based `branch` tool (§11). It finds the calling line's BranchLine per call, the way WorkspaceTools finds FileReads |
| **BranchLine** (new) | Branch | Yes | One line's view of its lines. Its role (root, tool line, user line or plugin line) and its parent come from `TreeCommands.info(own id)`. From its own facts it folds its open tool lines and the state of its current turn. It writes the roster, the idle notice and the ended notice, and the one status note a line receives when it becomes the default branch or stops being it. With two openers the roster is its own key, `branch:roster`, which holds the open tool lines it opened (§7.1) |

BranchLine injects `B:Dispatcher`, `B:TimelineIdentity`, `B:BranchLifecycle` and `S:TreeCommands`, and provides `B:BranchLine`.

It decides the way Goal does: from consumed state, through an owned write task that rechecks before writing, and only on live events after its load cutoff. A reload therefore writes nothing twice, with one exception: a load whose own history ends with a live roster key the fold does not match writes the roster once, because a restart that completed an ended line (§20.7) recorded `branch/ended` before `main`'s replay, so no live event follows it (§6.2).

The same exception reconciles a move that stopped between its fact and its notes. `branch/default` and each status note are separate batches (§4.5), so a daemon that stops after the fact commits leaves the fact in force and a note unwritten. On load, BranchLine compares two things for its own line: the `default` attribute of the latest live `branch:line` block on it, where a missing attribute means not default, and whether `TreeCommands` reports this line as the default branch now. When they differ it writes the note that matches the tree once, before the line's next model request, and a root never writes one. The new-default note names the line the move started from, so a reconciling load has to know that line: `branch/default` carries it, under the `from` field the note already needs (§16), which is what §4.9 admits a fact for, since the reconciled note decides with it. A load that finds its line's block and the tree in agreement writes nothing, so a second restart writes nothing. On the line the tree reports as the default branch alone, the comparison also reads the text of that block, so a note an older rule set wrote is replaced once at load.

**Restoring the default branch.** A Session whose directory holds one line is on its root and reads nothing for it. A Session with more lines reads one fact: the newest `branch/default` or `session/forked` among the root's own events, through the existing `timeline_events_by_kind` index. A `branch/default` whose target is still open and can be selected names the default branch; a `session/forked` means the default branch is the root, because every `branch/default` the fork copied was written before its `session/forked` and is void; nothing recorded means the root. A target that is no longer open, one naming a plugin line and one naming a line created through a client before the upgrade all fall back to the root (§4).

That one answer serves every request that omits a branch. A pending approval and an already created scheduled job carry the line they were bound to, so neither is resolved again (§9, §13).

An explicit creation API `at` is a fixed sequence, including when it equals the
parent's current head. Its fact records `at={kind: "sequence", sequence: at}`;
parent activity while validation awaits cannot widen the inherited prefix.
A `before` producer may still explicitly choose `at={kind: "create"}`, whose
existing meaning is the creation fact's own position.

Ended display details read purpose/source and end data at their recorded positions. The optional historical cut message count is omitted; displaying a branch does not count all preceding user inputs.

## 6. Messages between lines

A message is an `input/message` on the receiver's Timeline, written through `TreeCommands.input`. No new input path is added. A message carries one raw block, marked by its producer and without a key, and the mode of [InputQueue](../architecture/rust-agent/plugins/InputQueue.md):

| Mode | Effect on the receiver | When to use it |
| --- | --- | --- |
| `append` (default) | Rides with the receiver's next model request. It never starts a turn and does not wake `wait` | Progress or context the receiver should have at its next step |
| `steer` | Enters the receiver's current turn at its next step, or starts a turn if the receiver is idle. It wakes `wait` | The receiver must act on it now, or the receiver is idle |

Queue is not exposed. The only Queue input one line gives another is the opening input.

These are Collaboration's message modes and words (`append`, `steer`), with the same meaning, because both are InputQueue's modes.

- **Marker.** `<dim-branch-message from="C" to="P">`. The receiver tells a message from the user's words by its marker. RPC accepts only operator content, never blocks, so a client cannot forge a message.
- **Allowed pairs.** One rule, applied to the edge a line stands on: the line that opened a target, whatever it is, where the earlier change said `main`.

  | Pair | Message | Read | Refusal |
  | --- | --- | --- | --- |
  | The opener and each open tool line it opened | Each may message the other | Each may read the other: the opener reads the line it opened, and that line reads the opener from its cut | none |
  | A line the user opened and the line it was cut from | The line the user opened may message it | The line the user opened reads it from its cut; the line it was cut from neither messages nor reads it, because the user opened that line and it did not | the line it was cut from messaging or reading it: `branch_not_kin` |
  | The opener and a tool line it opened that has ended | Refused: it takes no input, and it answers nothing itself | The opener reads it, open or ended, because reading it is how the opener gets its result | `branch_ended` |
  | A plugin line and the line that opened it | Neither direction | The plugin line reads the line it was cut from; no line reads the plugin line through the tool | the message, and the opener's read: `branch_not_tool_line` |
  | Any other pair: siblings, a line and its grandparent | No | No | `branch_not_kin` |

  A line the user opened is a conversation line like a tool line, and the row it falls under is decided by the edge it stands on: as a line that was opened it has the second row, whose opener is the user and not the line it was cut from, and as an opener of tool lines it has the first row like any other opener. An ended tool line keeps its row, with its message refused: its opener still reads it, which is how the opener gets a result the line never sent. An ended plugin line stays under the plugin row, where no line reads it through the tool. `end` is not part of any pair: `end {content}` has no target, so it always ends the calling line (§10, §11). Coordination follows the parent-child relation, so it stays with the line that opened a line and never moves when the default branch moves, and a former default branch keeps the tool lines it opened and still messages and reads them.
- **Read.** `read {branch, after?, limit?}` returns one page of the target's committed history, open or ended, read from the store through `TreeCommands.page` and rendered the way Collaboration's `read` renders a member's page: inputs, text, tool calls and results, and turn ends, with `nextCursor`; a tool result item carries the effects its tool declared on that result (session boundary A.6), present only when the result declared at least one, a path verbatim and a command cut at its 200th character with the cut marked. The target is the line the caller was cut from, or a line the caller opened. One cursor rule covers both directions:
  - With no `after`, a read continues after the caller's last successful read of that line: the `nextCursor` of the latest read that returned a page. With no earlier read, a read of the line the caller was cut from starts at the caller's cut, and a read of a line the caller opened starts at that line's first event.
  - An explicit `after` reads from that point. A read of the line the caller was cut from raises a value below the cut to the cut, because the events up to the cut are already the caller's own history. A read of a line the caller opened uses the value as given, so `after: 0` re-reads that line from its start.
  - Every successful read moves the cursor to its `nextCursor`, an explicit-after read included; the latest read wins. The cursor is a fact of the caller's own line: BranchLine retains live read results in memory; after a reload it locates the latest successful read for the requested target only when a cursor is used, then reads that request's result fields. Resume does not replay tool history.
  - A page may end in the middle of the target's turn: it reads up to the target's published head, and the next read continues after that page.
- **Knowing when to read.** A read is on demand, so a line also needs to know its parent moved. The keyed `<dim-branch-parent>` block (§4, §7.1.1) states the parent's head and the line's cursor; the line decides which pages to read from it. The block never carries content, and reading it is not reading the parent.
- **No caps.** There is no limit on the number of lines, messages or steers. A message loop ends through the prompt rules (§12), through a line ending itself, or through the user (interrupt or end). Why this is safe, and who would own limits, is in §6.1.
- **Idle notice.** When a tool line's turn started in this generation ends, BranchLine on that line writes one steer to the line that opened it (`<dim-branch-idle>`, §12.1), unless one of the following holds:
  - the line was created with `idleNotice: none`;
  - the line is the default branch;
  - the turn already sent its opener a steer message;
  - the turn called `end`;
  - the line has a driving input pending.

  The second bullet keeps a line that has become the default branch from waking its opener every time the user talks to it there: it is answering the user, not reporting to its opener. A second move restores the notice without any write, because the rule is read from the current fact and not from a note. This way a line that stops without telling its opener never leaves the opener waiting on silence, and a line that already steered it does not notify it twice. The notice carries the turn's status (`completed`, `failed` or `cancelled`).
- **Waiting.** An opener waits for its lines with the existing `wait` tool. A steer message, an idle notice or an ended notice wakes it ([wait.rs](../../crates/dim-agent-capabilities/src/wait.rs)).

### 6.1 Resources and limits

Leaving caps out is safe because a line adds no new kind of resource:

- A line's work is ordinary turns of the same Session, under the same provider, Permission and Approval.
- A line runs at most one turn at a time. Loop holds one turn per line ([agent_loop.rs:106](../../crates/dim-agent-capabilities/src/agent_loop.rs)).
- A message is one bounded `input/message` written by a tool call, so the model's own output bounds it.
- RPC admission is bounded per connection ([architecture](../architecture/README.md)).
- Collaboration has no member cap either.

Open tool lines are capped. The cap is a total over the Session, and one rule decides what it counts:

- it counts the lines the `branch` tool opened that are still open, across the whole tree, and the tool reservations that will become them (§6.1, "A reservation spends the slot");
- it does not count a line on the path a close cannot take: the current default branch and every ancestor of it. That path is protected from `end` anyway (§10), so counting it would spend slots on lines that cannot be closed, and a user who keeps continuing on tool children would fill the cap with the very line they are working on;
- a line the user opened through `session.branch_create` never counts, and is never refused by this cap. A plugin line never counts either.

What the counting rule means for one open is decided by the new line, not by its opener. A line a `create` opens is new, so it is neither the default branch, which a move sets on a line that already exists, nor an ancestor of it, which is a line above the default branch. Every new child is therefore off the protected path and spends one slot, whether its opener is the root, the default branch, or a former default branch acting as coordinator. Being on the protected path changes what a line costs, not what opening from it costs. A move is what changes the count, and the count it produces is the same set difference over the new standing of the tree: the open tool lines minus the new protected path, computed for every open tool line at once, not for the two lines that gained and lost the role. So the line that gains the role stops counting, and every tool line below the line that lost it starts counting, however deep the subtree is. Updating only the two participants would count one line where the tree now protects none, and admit an open the Session has no room for. A move closes nothing and refuses nothing that already exists. So a switch that puts a line back on the counted side can leave the Session over the cap, and the next `create` is what refuses, with `branch_limit_reached`. Both of the cases the model records are that same rule. With the cap at two and one open tool line while the root holds the role, making that line the default branch takes the counted total from one to zero (`BranchEligibility-MoveToChild`). Starting at the cap with a nested tool line on the protected path and moving back to the root makes the formerly protected tool ancestors count again, admits the switch, leaves the Session over the cap and admits no further open (`BranchEligibility-AtCapBelow`). The nested case over a whole Session is `BranchCapSwitch`: with the cap at two and `main → A → B` the only open tool lines, selecting `B` protects both, so the count is zero, and switching back to `main` makes `A` and `B` count together, which is two, so the next `create` is refused (`BranchCapSwitch-BackToRoot`). One line opened meanwhile leaves the Session over its cap by the same switch (`BranchCapSwitch-OverCap`).

BranchTool owns the limit and refuses `create` with `branch_limit_reached` when an open would cross it. The refusal is about the new open only: a switch that leaves the Session over the cap closes no line and refuses nothing except later opens, since a line the user continued on did not become protected by anything the user asked to close. The refusal text says both things, so a model that meets it can report the state rather than retry.

**A reservation spends the slot.** A `create` reserves its line, under the tree lock, and that step is where the cap is charged: one lock covers the caller's eligibility (§4), the open tool lines and the admitted tool reservations. The reservation then waits for the build on it ([timeline_tree.rs](../../crates/dim-agent-capabilities/src/timeline_tree.rs)), and a caller that stops waiting leaves that build running. Counting published lines alone would let two calls take the last slot, one of them on a line that has not published yet. Publication turns a reservation into an open line and charges nothing a second time; a creation that definitively fails releases it; a caller that only stops waiting keeps it, because the build goes on. A check therefore reports the same counted total before and after a publication: the slot is spent once, at the step that admits the call.

If limits are ever needed for anything else, each one already has an owner, and none of them needs a mechanism change:

| Limit | Owner |
| --- | --- |
| Open tool lines | BranchTool, as a total over the Session: the caller's eligibility, the open tool lines and the admitted tool reservations are charged together under the tree lock when the creation is reserved, and publication charges nothing again (§6.1); the default branch and its ancestors do not count |
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
- Open tool lines grow their opener's roster, which lists only its open lines. Each end of such a line from outside it adds one notice on the line that opened it.

A tool line that never ends is the responsibility of its own protocol (`end`) and of the user, and its opener's roster keeps it visible. Goal ends Pursuit's evaluators ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)).

### 6.2 Known costs

- **A restart can lose an idle notice.** If the process stops between a tool line's turn end and its idle notice, the notice is never written, because BranchLine decides only on live events after load. Pursuit §4.2 leaves the same window open. The opener still has the line on its roster and can `read` it, and the user sees it idle.
- **A restart can lose the ended notice.** The same window between `branch/ended` and the roster input drops the ended notice a line's outside end would have written. The load heal writes the roster alone, so the opener learns it from the roster's retirement rather than from a notice.
- **A roster is short after compaction.** After a line compacts, its roster keeps only each line's id and the first line of its purpose. What an opener told a line survives only in the compaction summary or through `read`, and `read` brings that line's raw events into the opener's context, one page at a time.
- **A line that became the default branch keeps its history.** Making a conversation line the default branch changes what the line is told from then on (§9), but nothing it already recorded. Its earlier identity block and its purpose stay in its history, and the status note that says it is the default branch restates that identity under the same key, purpose included (§12.1), so the two never disagree and a compaction replays the note.
- **A note written by an earlier move can be stale.** A note states what holds while it is the line's latest block. A later move that writes nothing to that line leaves the earlier note in place, so no note says whether some other line is the default branch: that changes without a write to this one. What a note does state about the role is the same rule the tree applies: whether this line may open a line, whether it sends idle notices, and whether it may `end` itself, all of which the note's own case fixes. Protection of the default branch's chain is different in kind: it changes with another line's move, so it is decided when `end` is called, from the tree (§10).

## 7. Identity and context

### 7.1 Keyed state: `branch:line`, `branch:roster` and `branch:parent`

Every line answers "which line am I" with one block under the key `branch:line`. The key is the same on every line. A line that has opened tool lines also answers "which lines are mine" with a block under its own key, `branch:roster`. The two keys are separate because a line that opens a line and is not the root now carries both: one key would let a roster write retire that line's own identity, and a status note would retire its roster. A line also keeps one keyed block, `branch:parent`, for the line it was cut from (§7.1.1).

- **On a child: its identity.** TimelineTree writes it on every child, tool line, user line or plugin line. It is the first block of the child's opening Queue input, keyed `branch:line`, and it states:
  - the child's id, its parent and the cut;
  - that the child is the same agent and the history above is its own;
  - that it works in parallel from here on;
  - that messages above addressed to the parent, such as the roster, belong to the parent.

  A tool line's identity also carries its purpose (the `create` prompt) and the line protocol. For `idleNotice: none`, TimelineTree includes `idle-notice="none"` on that opening identity. BranchLine reads it from the line's own opening input when an idle notice is considered; resume does not replay inputs. A missing attribute selects `steer`, including histories written before the option existed. The setting controls only automatic idle notices. On a user line or a plugin line, the creator's prompt stays the opening input's `user` content, after the block.
- **On an opener: its roster.** BranchLine on a line that has opened tool lines writes a keyed Append, `branch:roster`, from that line's own facts:
  - `branch/create` with a tool source;
  - `branch/create-failed` of one of those lines;
  - `branch/ended` of one of those lines.

  The roster lists each open line it opened with the tool, with the line's id and the first line of its purpose, and is retired when none is open. A plugin line is never on a roster, so a Goal evaluator never appears in one. A line the user opened is not on one either: it stays in the Session's branch directory, where the user and the client see it (§14), and it lists the tool lines it opens on its own roster.
- **Superseding.** The two keys retire independently. A child inherits its parent's roster in its prefix, and its own roster, when it writes one, replaces that inherited block under its own key; its identity never touches the inherited roster, and the inherited roster never retires its identity. From the child's opening input on, the latest live `branch:line` block on the child is its identity and the latest live `branch:roster` block is its own roster. An inherited roster is retired rather than left to stand in for one the child has not written: TimelineTree reads whether `branch:roster` is live in the keyed state of the prefix the child inherits, with the same lookup it already does for `branch:parent` (§7.1.1), and when it is, the child's opening input carries one raw block after the identity, keyed `branch:roster` and retired. A child that opens no line of its own therefore holds no roster at all, instead of a parent's list of lines it never opened; a compaction of that child replays the retirement and not the inherited block. A child cut from `main` before `main` wrote a roster inherits nothing and gets no such block.
- **A status note is a later block of the same key.** When a line becomes the default branch, or stops being it, BranchLine writes one keyed Append under `branch:line` on that line, carrying `default`. Because it is the identity's own key, it replaces the birth block, so the note restates that identity byte for byte and changes only the sentences the move changes (§12.1). The root writes no note: its two rules, that it opens lines and that it never ends, are the same whether or not it is the default branch, so its roster keeps the key alone and a later compaction of the root replays the roster unmixed. A note is written under `branch:line`, and a roster under `branch:roster`, so a note never retires a roster, whichever line holds one.

Key and mode are independent ([history.rs](../../crates/dim-agent-capabilities/src/history.rs) `render_input`). Goal's `set` already puts keyed blocks in a Queue input ([Goal §5](plugins/Goal.md#5-context-mapping)).

The identity is part of the opening input, not an Append written at `branch/start`, for two reasons:

- An Append can commit after the opening input ([Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal)).
- The purpose is not known at `branch/start`.

### 7.1.1 The parent block, `branch:parent`

A read is on demand, so a line must also know that the line it was cut from moved. BranchLine on that line watches its parent's turn ends and writes one keyed Append, `branch:parent`, on its own Timeline. It is the line's own fact and never the parent's: no part folds another line's events.

- **The signal.** TimelineTree already routes every line's `turn/end`; the line's node publishes the sequence of its latest turn end. A child's BranchLine waits on its parent's signal and on its own write queue, and a change queues one decision. The decision is the line's state, not the signal: whenever the line holds no `branch:parent` block and the parent's head is past the head the latest block told, it writes one. A move that arrived while a block was held is written when that block is consumed, deleted or interrupted, so no move is lost. No part reads the parent's history for this.
- **The condition.** The block is written only when all of these hold:
  1. the line is not the root and is not ending;
  2. no earlier `branch:parent` input is still held on the line, meaning committed and not yet consumed;
  3. the parent has new activity past `from = max(A, told, opened)`, where `A` is the line's read cursor for the parent (its cut when it has not read yet), `told` is the head the latest `branch:parent` block on the line reports (0 when there is none), and `opened` is the sequence of the line's `branch/create` event on the parent, at or above every cut. Activity from before the line existed is not news, and the line reads it when it needs it.
- **What counts as new activity.** Two kinds of event count: an operator input, an `input/message` that carries `user` content, and a tool call. The creating call's `step/tool-use` is written before `opened`, so it is never counted. Nothing else counts on its own: the parent's text, its turn ends, block-only inputs such as the roster and messages from lines, and the creating call's result. The rule is that a line is told when the parent acts or when the operator tells the parent something, and never for the turn that only opened it (its creating call's result, its text and its turn end), which would make every line read its parent for nothing and train it to ignore the block.
- **The check.** The parent's facts are read through the one event query ([ADR 019](../adr/019-event-query.md)): the branch's `input/message` and `step/tool-use` events, newest first, paged until one qualifies or the read reaches `from`. No other line's events are folded.
- **The write.** One keyed Append, `branch:parent`, written on the line itself through `TreeCommands.input`, with the parent's id, `head` and `after` as attributes. A later block of the same key replaces the earlier one in the keyed baseline. After the write the fold records the new head and the new input, so a second decision in the queue does not write again.
- **At open.** When a child's inherited prefix holds a live `branch:parent` block, the child's opening input carries one more raw block, after the identity block: keyed `branch:parent`, live and not retired, naming the line that opened it P with `head` and `after` both the cut H. It supersedes the inherited block the way the identity supersedes the inherited roster (§7.1), so the line's latest `branch:parent` always names its own parent and its `told` starts at H, which equals `opened`; a compaction replays this block and never the one above it, and the usual block supersedes it when P moves. Only a nested line gets it: a line cut from `main` inherits no `branch:parent` block, so its opening input is byte for byte what it was. TimelineTree composes it where it writes the identity block, and reads whether the key is live from the keyed state of the prefix the child inherits, never from the rendered history. It reads that state while it opens the child, as it reads a resumed line's facts; a failed read fails the create like any other opening failure (branch/create-failed).
- **What follows.** A running line gets the block with its next model request. An idle line is not woken: the block is consumed with the first request of its next turn, ahead of the input that starts that turn, which is the documented Append rule ([session access](contracts/session-access.md), [Loop](../architecture/rust-agent/plugins/Loop.md)). A reload writes nothing; a restart writes at most the one block that was due. `main` gets no block: nothing is above it.
- **The record.** The line sends the System its configuration holds. The guide is the same on every line (§7.3), so a line whose System did not move from the one it inherited references the configuration that recorded it, on whichever line of its chain recorded it, and a line publishes its own `context/configured` when its System moves, as any branch does.
- **Rejected alternatives.**
  - Writing at the line's own turn end misses the case the change exists for: the line ends a turn, the parent moves on, and the user or the parent then talks to the line. That turn carries no block.
  - Writing at the line's turn start or between its requests reacts to an event after the request was prepared; a committed Append can land after the request it was meant for, and it would need a hook in Loop.
  - `main` pushing a summary or a steer to every line duplicates content the line can read and wakes idle lines.
  - A new event kind or a stored format change is unnecessary: a raw keyed input already carries the state.

  The block states positions and no content; the content stays in the pages a read returns.

### 7.2 Compaction

Compaction is per line. A compaction is a `context/compaction` fact on the compacting line's own Timeline.

- A child inherits its ancestors' compaction records up to its cut. Event ids are stable across the inherited prefix, so `retain_from` replays the same way on the child ([ContextCompaction](plugins/ContextCompaction.md)). After the cut, the child compacts on its own.
- A child's compaction never changes its parent, and a parent's compaction never changes a child.

The keyed baseline replays the latest live block of each key from before `retain_from`. As a result:

- when `main` compacts, its roster replays;
- when a child compacts, its identity replays, and its roster replays only when the child wrote one: an inherited roster was retired at its open (§7.1);
- when a line compacts, its latest `branch:parent` block replays, so the block survives a compaction of the line.

Compaction deletes no rows: the line's fold of its read cursors and of the latest block is rebuilt from the same events on replay.

The `branch:line` key means every line's baseline holds exactly one answer to "which line am I". An inherited answer can never outlive the line's own. Messages carry no key: after a compaction they survive in the summary or in the retained turns, like any other input.

### 7.3 Cache prefix

Lines differ only in their tails:

- The root adopts system and model, and so does a line that is the default branch when it has its own inherited prefix. A line inherits its configuration from its cut ([prompt](../architecture/rust-agent/prompt.md)).
- Prompt sources load with the root's scope and are inherited ([PromptSources](../architecture/rust-agent/plugins/PromptSources.md)).
- One ToolRegistry per Session gives every line the same tool list. That is why `create` on a line that may not open one is refused through the tool result instead of by removing the tool.
- The request marks that prefix for cache where the protocol caches only what it is told to: up to two breakpoints, at the end of the System and on the last block of the last user or tool-result message, each placed only when its conditions hold ([Anthropic cache breakpoints](../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)).
- The identity block is the child's first event of its own, directly after the inherited prefix. A child's first request therefore reuses the parent's prefix up to the cut.
- Roster updates are Appends at the end of the opener's history.
- Parent blocks, and the status note a move of the default branch writes, are Appends at the end of the line's own history, so they never move any line's inherited prefix. Making a line that is already open the default branch therefore leaves its cached prefix unchanged: only its tail grows.
- A status note is an Append at the end of the line's own history too. Making a line the default branch therefore leaves that line's cached prefix unchanged: only its tail grows.
- The guide is the same on every line, including the parent block's entry.

### 7.4 Goal and Pursuit

**Rule.** A line's opening input is TimelineTree's write. Its first block is the `branch:line` marker, so under [Goal A5](plugins/Goal.md#appendix-e-assumptions) (operator input is "`user` content and no source marker as first raw block") it is a sourced input, not operator input, whatever `user` content the creator gave. Goal's conflict table records this ([Goal Appendix B](plugins/Goal.md#appendix-b-full-conflict-table)). Goal's own rules are unchanged. Its code changes only through the [Pursuit amendment](plugins/Pursuit.md#13-ending-the-evaluator-with-427).

Before this design, a plugin line's opening input (the creator's prompt as `user`, no block) counted as operator input. A tool line's did not: it had the `dim-branch-task` block and no `user`. Both are now sourced inputs.

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

- An opener gives each line it opens its own files or area in the `create` prompt.
- A line tells its opener before it changes any file outside its area.
- On `file_modified_since_read`, the line reads the file again and tells its opener before it continues. A line that has become the default branch may meet this failure on its first edit of a file the earlier default branch changed: that is the guard working, and its status note says so (§9).

The only file-change fact a tool declares is `fileWritten`: `edit` and `write`
declare one in the result's `effects`, with the path, whenever they published
the file — on success, and on a failure after the publish, such as the
parent-directory sync. A call that fails or is cancelled before the publish
declares none ([Session boundary A.6](contracts/session-boundary.md#a6-tool-effects)).
A client that folds those declarations gets a partial set by construction — it
misses files changed by `exec`, by an MCP tool, or outside Dim — so the fold
describes tool-declared writes, not the workspace's changes. No method promises
a complete change set for a turn or a line.

WorkspaceTools is unchanged.

## 9. The user

- The interaction surface belongs to the Session. Every line deals with the user as this Session.
- **Questions.** Any line can ask with `ask_user`. The answer returns as that call's tool result on the line that asked ([Questions](plugins/Questions.md)).
- **Approvals.** Approvals for a line's tool calls go straight to the user. Approval notices carry `branchId`, and clients label the line.
- **Nothing is relayed through the default branch.** It is the same subject asking its own user. A pending approval stays on the line that asked for it: its notice carries that line's `branchId`, and a later move of the default branch neither moves the request nor changes the line a response reaches (§8, §13).
- **Submitting.** The user submits to any open line (`session.submit` with `branchId`). A call that omits `branchId` goes to the default branch.
- **Interrupting.** Interrupting a line ends its turn, not the line. A stop that omits `branchId` stops the default branch.
- **Ending.** The user ends any line except the root, the default branch and every ancestor of the default branch (`session.branch_end`, §10). No caller can end the default branch, the user included: to end it, the user moves the default branch to another line first. A line the user opened is ended this way, never by its own `end`.
- **Choosing the default branch.** The user makes any open conversation line, a tool line or a line they opened, the default branch, or moves back to the root, with one method (`session.branch_set_default`, §14). The root is a valid target whenever it is not already the default branch, and a plugin line never is. The old default branch stays open and keeps the lines it opened; nothing is copied and no history moves. The move writes the fact on the root and gives each of its participants a status note unless it is the root: the new default branch and the old one, so a move back to the root still writes the note the line losing the role needs (§12.1). It also moves the right to open a line: leaving the role costs the old line that right and keeps its lines, and taking it gives the new one that right. The root's right is not part of that exchange, because it holds the right whether or not it holds the role, so nothing a move does takes it away.
- **What the move does not do.** It changes which line the Session's conversation is on. It does not touch the workspace, so a file the earlier default branch changed stays changed; it does not move `/fork`, which still copies only the root to a rest point ([fork](appserver/session-deletion-and-fork.md)); and it does not end either line.

## 10. Ending a line

| Event | Timeline | Data | Role |
| --- | --- | --- | --- |
| `branch/end` | The line that ends | `{source, ancestor?}` | Command |
| `branch/ended` | Its parent | `{branch, endEvent, source, ancestor?}` | Result, written by TimelineTree |

This pair mirrors `branch/create` and `branch/create-failed`: a command on one Timeline, routed by TreeTimeline, and a result from the tree. `endEvent` is the sequence of `branch/end` on the ending line. `branch/ended` carries this result on the line that opened the ending line, `main` when `main` opened it, so a case that names a line holds below an opener that is not the root.

`source` is the existing `BranchSource`, the same type `branch/create` uses, so a create and an end record their issuer the same way:

| Source | Issuer |
| --- | --- |
| `{kind: "tool", turn_id, request_id, call_id}` | A tool call on the ending line itself: the `branch` tool's `end` |
| `{kind: "host", operation_id}` | A host operation, either App Server's `session.branch_end`, which mints `operation_id` the way `session.branch_create` does (a uuid, [connection/tree.rs](../../crates/dim-app-server/src/connection/tree.rs)), or a plugin with its own id, such as Goal's `goal:evaluate:N`. A user end records the `{kind: "user", operation_id}` variant it recorded at creation |

`ancestor` names the line whose own end started the cascade: the end that carries no `ancestor` of its own. Every descendant the cascade ends carries that line, `source` is copied from that same end, and the pair therefore names one end operation: who asked, and on which line. A line ended at its own request has a `source` and no `ancestor`, and a descendant whose own end was already committed keeps that pair when the cascade reaches it. `branch/ended` copies `source` and `ancestor` from the line's `branch/end`, and is always written on the ending line's own parent, taken from the directory.

**A general operation.** `TreeCommands.end(id, source)` and `Session::end_branch(id, source)` check only the target: it exists, it is open, it is not the root, it is not the default branch, and it is not an ancestor of the default branch. Ending the root would close the Session, and ending a line ends every line below it, so the default branch's whole chain is protected. These checks are about the target, not the caller. They never check who calls: the user, an opener and a plugin meet the same guards, and ending the default branch always takes a move of the default branch first.

The check reads the chain from the directory at the moment of the call and under the tree lock that a move also takes, so a move and an end cannot interleave into a state the rule forbids. Two orderings are therefore admitted, and nothing else: either the end is admitted and the move is refused, because the target or one of its ancestors is ending or ended, or the move is admitted and a later end of a line on the new default branch's chain is refused. A move is refused with `branch_target_protected` when the target or any ancestor of it is still `ending`, so a target cannot become the default branch while an ancestor's cascade is already writing its descendants' ends. A refusal writes nothing.

**The tool's end is one operation, and its final message survives a stop.** The `branch` tool's `end` ends the calling line and leaves its parent a final message (§11). The two writes are on two Timelines, and `TimelineStore::append` commits a batch for one Timeline key, so no batch can hold both: holding the tree lock orders them in memory only. The order is therefore the one that stays recoverable, under that same tree lock:

1. check the guards;
2. commit `branch/end` on the caller, **carrying the final message**;
3. then deliver that message to the parent as a steer, from the end.

A move that reached the lock first refuses the whole call, so no message is written for a line that is still open; an end that reached it first refuses the move, and the parent receives the message of a line that really ended. A daemon that stops between the two writes leaves a line that has ended and a parent that has not been told; restore delivers that message (§20.26b).

**No double delivery.** The steer's submission id is derived from the end's `source`, `branch-final:<line>:<call id>`, so the source recorded in `branch/end` makes it the same id after a restart. `Timeline::input_checked` accepts each submission id once ([timeline.rs](../../crates/dim-agent-runtime/src/core/timeline.rs) `input_checked`, `submission`): it looks for an `input/message` with that id on the receiver's own Timeline, stored and still unpublished alike, and answers the existing sequence with nothing written. After a restart that lookup runs through the restored head, which is the store's `head` for that Timeline, so a message committed before the stop is found and a second attempt writes nothing. What one id cannot cover is the other write: the end and its delivery are on two Timelines and are never one batch. That is what the order above buys, and what restore completes.

**Restore.** A line whose own `branch/end` is a tool end carrying a final message is owed one delivery to its parent, whether the stop came before or after the delivery. Beside the completion of an ended line (§20.7), before the parent's next model request, restore delivers it through the same `input_checked` call with the same derived id: one lookup by submission id, a write when the message is missing and nothing when it is there, so a restart after a completed delivery writes nothing and a second restart writes nothing at all. `branch/ended` on the parent is written after the close, therefore after the delivery, so a line whose result the parent holds is never owed a message again.

- Ending a line that has already ended writes nothing, and the reply is `branch_ended`.
- A line still opening is not a line yet: its creation can still fail, and one terminal fact per line is the rule, so `end` answers `unknown_branch` until the line is published. An opener that ends at load waits for the Session, which publishes after its restore (§17 R1).
- Closing the Session is not an end.
- "The opener manages its line" is a convention among callers, not a runtime rule:
  - the `branch` tool's `end` takes no target, so it ends the calling line, by the tool's design; a line that is the default branch is refused, because the Session's conversation is on it;
  - a parent that wants a child to end asks it with a steer, which is the tool's convention.

Sequence:

1. The entry commits `branch/end` on the line. From then on the line accepts no new input and opens no line, and no line opens under any line below it: those entries fail with `branch_ended`. A line below takes input until its own `branch/end` (step 3).
2. TreeTimeline on the line consumes `branch/end` and hands it to TimelineTree.
3. TimelineTree ends the line's descendants first, through these same steps, each with `source` copied and `ancestor` naming this line, however deep it is.
4. The close happens:
   - for a `tool` source: at the line's first turn end after `branch/end`, because the call is part of that turn, or at once if the line is idle;
   - for a `host` or `user` source, and for a line ending because of an ancestor: at once, interrupting the current turn.
5. `Branch::close()` interrupts the line, settles its work and unloads it ([branch.rs](../../crates/dim-agent-runtime/src/branch.rs)).
6. TimelineTree writes `branch/ended` on the parent, last.

On the parent, BranchLine acts only for the tool lines it opened; plugin lines belong to their creators, and a line the user opened is on no roster and is ended by the user, so the line it was cut from writes nothing for it (§6, §7.1):

- When such a line ended itself through the tool (a `tool` source and no `ancestor`), it writes the roster update only. The final message is delivered from the line's own `branch/end`, as the paragraphs above state, and restore delivers one that a stop left out.
- For any other end of such a line, it writes the roster update plus the ended notice (§12.1), as a steer, in the same input.

Clients show "ended itself" for a `tool` source and no `ancestor`, and otherwise the operation that ended the line; a line ended by a cascade names its ancestor, for example "ended with X".

**Restore.** A child is inactive when its parent has a `branch/ended` for it. The resumed parent's history read reports these (§5). This joins the existing rules: the parent is inactive, the parent has a `branch/create-failed` for the child, or the child's creation never completed.

A line that has `branch/end` but whose parent has no `branch/ended` for it is not opened. Restore completes such ends deepest line first, writing each missing `branch/ended` on the parent with `source` and `ancestor` taken from `branch/end`, and a line's own result last. For a line below it, that parent is not opened either, so the record joins its stored history and its fixed head follows. A line below one of them whose own end never started gets no `branch/ended`: the parent is closed and never replays, so nothing would read it. Restore records each started descendant as ended at the head it stopped at; creation source is read only by an explicit detail request, and its history stays readable (§1, "After the end").

**After the end:**

- The line stays in the branch directory, and its history stays readable from the store, through `session.history` (v3) and through the tool's `read`. Its head never moves.
- A fork copies directory rows and main's events as it does today. A line that ended before the fork point is inactive in the fork too, because its `branch/ended` is in the copied prefix. A fork's default branch is its root (§13).
- Deleting the Session removes the line along with it.
- Session close and idle unload are not ends: lines restore as open.

**Plugin lines.** Each opener ends its own lines under its own protocol. Goal ends its Pursuit evaluator once the evaluation is settled, and treats an outside end before the conclusion as `failed` ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)).

## 11. The `branch` tool

The tool takes one action-based input, which replaces `{prompt, at?}`. Its risk stays `session`, and Approval admits each call as it does today.

| Action | Input | Who | Effect | Result text |
| --- | --- | --- | --- | --- |
| `create` | `prompt`, `at?`, `idleNotice?` (`steer` \| `none`) | The root, and the current default branch | Opens a tool line cut at `at` (default: before this request's model start), which reports to the caller. Refused with `branch_limit_reached` when the open would cross the Session's cap, charged at the reservation (§6.1). The idle notice defaults to `steer`. | `Line created: C` |
| `message` | `to`, `content`, `mode?` (`append` \| `steer`) | A line to a tool line it opened, or to the line it was cut from | One `input/message` on `to` | `Sent to C.` |
| `read` | `branch`, `after?`, `limit?` (1–200) | A line on the line it was cut from or on a tool line it opened, open or ended. A line the user opened reads the line it was cut from on those terms; that line neither messages nor reads it, because the user opened it | One page from the store, from the caller's cursor when `after` is absent (§6) | The page as JSON: items, `nextCursor`, `done` |
| `end` | `content` | A tool line that is not the default branch, on itself | One `TreeCommands.end(C, source, final_message)` under the tree lock: it checks the guards, writes `branch/end` on C carrying the final message, then delivers that message (steer) to the parent under the submission id derived from `source` (§10). A refused end writes no final message | `Line C is ending; your final message was sent to P.` |

The input schema is `{action, prompt?, at?, idleNotice?, to?, content?, mode?, branch?, after?, limit?}`, with `action` required and `additionalProperties: false`. `idleNotice` is valid only for `create`.

Refusals come back as error results (code in `structuredContent`) and write nothing:

| Case | Text | Code |
| --- | --- | --- |
| `create` on a line that is neither the root nor the default branch | `Only the root and the default branch open lines. Tell one of them if more parallel work is needed.` | `branch_not_opener` |
| `create` when the Session already holds its cap of open tool lines, counting the admitted reservations | `This Session holds its limit of open lines. Continue on this line, or end one of its open lines first.` | `branch_limit_reached` |
| `message` or `read` outside kinship | `You can message or read only the lines you opened and the line you were cut from.` | `branch_not_kin` |
| `message` on a plugin line, or a line's `read` of one | `Line C was not opened with this tool; it follows its own protocol.` | `branch_not_tool_line` |
| `message` to an ended line, or a race with an end | `Line C has ended. Read it to see what it did.` | `branch_ended` |
| `end` on the root, on the default branch, on a plugin line or on an ancestor of the default branch | `Only a line opened with create can end itself, and not while it or a line below it is the default branch.` | `branch_cannot_end` |
| `read` with an `after` past the target's head | `after A is past line T's head H; a read starts at or below H` | none: an error result with no code |

These refusals are the tool's own rules. `TreeCommands.end` itself checks only the target (§10). An `end` whose line already committed its own `branch/end` takes the `branch_ended` row and writes no second final message: the first call's message is the line's last word. Because the guards, `branch/end` and the delivery are one operation under the tree lock a move also takes, a move that makes the target the default branch first is refused before any message is written, and the refusal above is the whole effect. An end that commits `branch/end` and stops before the delivery is not a second end to answer: the message is recorded in `branch/end` itself, and restore delivers it once (§10).

### Activity page execution

The `read` limit counts raw events and includes the complete final atomic batch. TimelineTree resolves
that endpoint from batch metadata before any display read. BranchTool then requests only input/text,
tool-start names, tool-call identities/names, result identities/error flags/effects and turn outcomes
inside that range. Model requests, tool inputs and result content do not belong to this activity view.
The cursor is the raw endpoint even when no event in the range produces a display item. Hidden events
still break adjacent-text merging. No event cache or additional storage format is introduced.

## 12. Prompts

### 12.1 Context mapping

`C` is the line, and `P` is its parent (`main` for a tool line). Content inside markers is escaped as TimelineTree escapes it today (`context_block`).

| Write | Where, key | Mode | Text |
| --- | --- | --- | --- |
| Identity, tool line | `C`, opening input, first block, `branch:line` | Queue | `<dim-branch-line branch="C" parent="P" source="tool">` / `You are line C of this session. Line P opened you at this point of its history to work in parallel with it. You are the same agent as P: everything above is your own history, and you continue from here on your own line. P keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to P, such as its list of open lines, are P's, not yours.` / blank / `Your purpose, as P wrote it:` / `<prompt>` / blank / `How you work with P:` / `- Work on this purpose only. You cannot open lines unless this line is the default branch of the session, and this block does not make it one: a status note would. If more parallel work is needed and you cannot open a line, tell P.` / `- Read P with the branch tool (action read, branch "P") when what P did or decided since your cut changes your work; the parent block reports when P has moved. Message P only for what it must act on: a decision that changes its work, a blocker only the user can settle, a conflict with another line, or a change to your purpose. Use mode "steer" for what P must act on now, "append" for what P should see at its next step. Do not send P a progress report it can read, and do not page through P's whole stream: read the page that answers your question, then act.` / `- You share the workspace with P and its other lines. Change only the files P gave you; before changing any other file, tell P which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell P before you continue.` / `- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.` / `- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. If you already told P that in a message, your end content carries only what is new. Read P once more before you end if the parent block says P has moved, so your last word still holds. That is your last action; P receives it.` / `</dim-branch-line>` |
| Identity, plugin line | `C`, opening input, first block, `branch:line`. The creator's prompt is the input's `user` | Queue | `<dim-branch-line branch="C" parent="P" source="host">` / `You are line C of this session, opened from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You can read P's work after this point. Messages above that were addressed to P are P's, not yours. The message after this one says what this line is for.` / `</dim-branch-line>` |
| Identity, line the user opened | `C`, opening input, first block, `branch:line`. The user's prompt is the input's `user` | Queue | `<dim-branch-line branch="C" parent="P" source="user">` / `You are line C of this session, opened by the user from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You can read P's work after this point. You cannot call end on yourself; the user ends this line. Messages above that were addressed to P are P's, not yours. The message after this one is from the user for this line.` / `</dim-branch-line>` |
| Roster | `O`, `branch:roster` | Append | `<dim-branch-lines branch="O">` / `Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it can read your work after that point, and you can read its.` / `- C1: <first line of its purpose, at most 120 characters>` / … / ``They message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you.`` / `</dim-branch-lines>` |
| Roster, none open | `O`, `branch:roster` retired | Append | `<dim-branch-lines branch="O">` / `All lines you opened have ended.` / `</dim-branch-lines>` |
| Roster, inherited, at a child's open | `C`, opening input, after the identity block, `branch:roster` retired | Queue | `<dim-branch-lines branch="C">` / `The list above belongs to line P and names the lines it opened. This line has opened none of its own, so it keeps no list.` / `</dim-branch-lines>` |
| Parent block | `C`, `branch:parent` | Append | `<dim-branch-parent branch="P" head="H" after="A">` / `Line P has new activity: it was at H when this was written, and you have read it to A. Read what is new with the branch tool (action read, branch "P"); with no after, the read continues from A. Read it when what P did changes what you do, and before your final message; do not page through all of P's work.` / `</dim-branch-parent>` |
| Parent block, at a nested line's open | `C`, opening input, after the identity block, `branch:parent` | Queue | `<dim-branch-parent branch="P" head="H" after="H">` / `Line P has no new activity since you were cut from it at H. Parent blocks above this point were written for P and are not yours.` / `</dim-branch-parent>` |
| Message | Receiver, no key | `append` or `steer` | `<dim-branch-message from="C" to="P">` / `<content>` / `</dim-branch-message>` |
| Final message | `P`, no key | Steer | `<dim-branch-message from="C" to="P" final="true">` / `<content>` / blank / `Line C ends with this message.` / `</dim-branch-message>` |
| Idle notice | `P`, no key, for a tool line `C` that `P` opened | Steer | `<dim-branch-idle branch="C" status="completed\|failed\|cancelled">` / `Line C's turn ended (<status>) without a message to you, and C is waiting. Read it if you need its state; message it with mode "steer" to give it more work, or leave it.` / `</dim-branch-idle>` |
| Ended notice | `P`, no key, in the roster's input, for any end of a tool line `P` opened other than its own `end` | Steer | `<dim-branch-ended branch="C">` / `Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did.` / `</dim-branch-ended>` |
| Status note, new default branch, a tool line | `C`, `branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="tool" default="true">` / the birth identity's sentences byte for byte, purpose label and purpose included, then blank / `This line is now the session's default branch, switched from line O. While this block is your latest one: answer the user here; you can open lines with create; you send no automatic idle notice to P; you cannot call end on yourself. Line O stays open and keeps the lines it opened.` / blank / `How you work here:` / `- The user switched here to continue directly from this line's context and approach, so the user's request takes precedence over the purpose above, which still says what this line is for.` / `- When your purpose is done, or cannot be done, message P with what you found, did, changed and left undone and keep working: while this line is the default branch you cannot call end on yourself.` / `- You share the workspace with P and its other lines. If edit or write fails because the file was modified after it was last read, another line may have changed that file: read it again before editing.` / `</dim-branch-line>` |
| Status note, new default branch, a line the user opened | `C`, `branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="user" default="true">` / its birth identity's sentence byte for byte, then blank / `The user has now made this line the session's default branch, switched from line O. While this block is your latest one: answer the user here; you can open lines with create. Line O stays open and keeps the lines it opened.` / blank / `How you work here:` / `- Carry this line's work forward yourself; the user's request takes precedence over the prompt this line was opened with.` / `</dim-branch-line>` |
| Status note, former default branch, a tool line | `C`, `branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="tool" default="false">` / its birth identity's sentences byte for byte, then blank / `The session's default branch moved from this line to line O. This line stays open, reports to P as before and keeps coordinating the lines it opened; it can no longer open lines with create, and the rest of the rules above hold again. Only a line opened with create can end itself, and not while it or a line below it is the default branch.` / `</dim-branch-line>` |
| Status note, former default branch, a line the user opened | `C`, `branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="user" default="false">` / its birth identity's sentences byte for byte, then blank / `The session's default branch moved from this line to line O. This line stays open and keeps coordinating the lines it opened; it can no longer open lines with create, and the rest of the rules above hold again. Only a line opened with create can end itself, and not while it or a line below it is the default branch.` / `</dim-branch-line>` |
| Tool description | Every line | — | `Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed by reading each other's work and messaging what the other must act on. On main you coordinate: answer a line's steer and act on what it asks for; leave a line's part to that line instead of redoing it; read a line's work instead of waiting for a report.` / `- create (the root and the current default branch only): open a line to work on prompt in parallel. Open one line per part that can go on at the same time as the rest — separate files, a separate question, or a second approach — instead of doing those parts one after another yourself; not for a single quick edit, nor when your next step needs its result. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Returns the line's id.` / `- message: send content to a line you opened, or to the line that opened you. Send a decision, a blocker, or a conflict, not a progress report the receiver can read. Mode "append" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use "steer" when the receiver must act now or is idle.` / `- read: show a page of the activity of the line you were cut from, or of a line you opened, also after it ended. With no after, a read continues after your last read of that line: from your cut for the line you were cut from, and from its first event for a line you opened. Pass after to read from another point. Do not page through a whole line.` / `- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.` / `A line messages and reads the line it was cut from and the lines it opened. The end action ends the calling line and every line below it.` |

These texts replace the delegation wording in the code today: the `dim-branch-task` block in `first_input` ("That branch delegated this task to you") and the tool description ("works on prompt as a task you delegate"). Their classes are in [§12.4](#124-texts-and-their-classes).

The product sets none of `BranchToolConfig`'s note members, so a shipped status note ends after the sentences the move changes: the `How you work here:` blocks in the table above are the design's first versions, and the three members supply them (§12.4).

**Why a status note restates the identity.** A note is a later block of the `branch:line` key, so it replaces the birth identity of the line that receives it, and a compaction replays the note alone: a line that could not name its parent, its source and its purpose from its own current block would lose them for the rest of its life. A note therefore repeats the birth block's sentences byte for byte and changes only the sentences the move changes. No note says whether some other line is the default branch, because a later move writes nothing to this line; what a note states are the rules for this line, and the rule it states about `end` is the tree's own, applied when `end` is called (§10).

**Which lines get a note.** The rule is per participant: each participant of a move that is not the root receives one note, and the root receives none. A move has two participants, the line the user moved to and the line the user moved from, so it writes two notes when neither is the root, one when one of them is, and none only when both are, which no move has. `main → B` writes one note, to B with `default="true"`; `B → main` writes one note, to B with `default="false"`; `B → C` writes two. The root never receives one in either direction, because neither of its rules changes: it opens lines and it never ends, whichever line holds the default branch. Every other line receives nothing, so no other line's cached prefix moves (§7.3). That is also why no fork-retire rule is needed: a fork copies the root's `branch:line` block, and the only such block the root can hold is its roster.

### 12.2 Items for the prompt layer ([#429](https://github.com/arcships/dim/issues/429))

The texts above are first versions, owned by TimelineTree's plugin. These items belong to the model-facing prompt layer:

- How an opener briefs a line, and what coordinating means for a line that may open lines: assigning areas, answering steers, ending its own wait, and not redoing a line's work. The text applies to both openers, the root and the current default branch.
- **Goal's wording (owned by Goal).** A line is not delegated work (§1): the objective block's sentence, the continuation block's sentence, and the statements in [Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal) and Goal §5's row for child branches. The texts are [Goal §A.5](plugins/Goal.md#a5-model-visible-texts)'s, and a change to either sentence is a policy change judged by [Goal §A.5.2](plugins/Goal.md#a52-evaluation). Goal owns these texts.

The requirements that answer these items are in [Model-facing prompts](model-prompts.md).

### 12.3 Evaluation scenarios

The set lives in the repository, one directory per scenario, holding a `scenario.json` and the starting `workspace/` beside it: sixteen under [`xtask/scenario-eval/timeline-branches/`](../../xtask/scenario-eval/timeline-branches/), each declaring a window of 131,072 tokens, and `identity-after-compaction` under [`xtask/scenario-eval/timeline-branches-compact/`](../../xtask/scenario-eval/timeline-branches-compact/), declaring 29,000 so that compaction happens. `budget.window` is a check, not a setting: a run whose Session declares another window fails before its first turn, so a rerun needs a model that declares exactly the scenario's window, and a model that declares none falls back to 128,000 and fails too. These are run locally on real models, and the results are reported in the implementation PR. CI asserts only behavior.

`main-moved-on` is the scenario for the parent block (§7.1.1): `main` works before the cut, opens a line, then keeps working; the line must read what `main` did after the cut before it answers or ends, and it must not page through all of `main`'s activity. It is the scenario for [§12.4](#124-texts-and-their-classes)'s parent-block policy sentence.

The opening and switching roles add eight scenarios: `long-work-and-a-flaky-test`, `refactor-and-a-pasted-report`, `same-file-tradeoff`, `background-command-not-branch`, `switch-continue-directly`, `switch-then-open-lines`, `root-opens-after-switch` and `user-line-becomes-default`.

`identity-after-compaction` reuses `redone-work`'s message and workspace, and `flooding` and `never-ending` share one workspace: each pair stays identical, and the no-model test that reads both committed directories fails when one copy changes alone.

| Scenario | Scenario directory | Watch for |
| --- | --- | --- |
| A line acts as the default branch | — | A line that is not the default branch claims the Session's conversation, ignores that it may open lines, or one that is the default branch stops reporting to its opener |
| A line opens where it may not | — | A line that is neither the root nor the current default branch opens a line instead of telling one of them |
| The cap and the path | — | A line refuses to open one while the Session is over its cap, or a switch closes a line to make room |
| Redone work | `timeline-branches/redone-work` | `main` repeats what a line is doing, or two lines do the same thing |
| Stale knowledge | — | A line acts on files or decisions that changed after its cut without re-reading them |
| Parent moved on | `timeline-branches/main-moved-on` | A line answers or ends without reading the activity its parent committed after the cut, or pages through all of its parent's activity |
| Flooding | `timeline-branches/flooding` | Append or steer messages that repeat what the receiver already knows |
| Over-splitting | `timeline-branches/over-splitting` | Lines opened for short or sequential steps |
| Never ending | `timeline-branches/never-ending` | A line that is done but does not call `end` |
| Sibling relay | `timeline-branches/sibling-relay` | An opener is used to pass messages between lines that should have been one line |
| Identity after compaction | `timeline-branches-compact/identity-after-compaction` | After compaction on `main` and on a line, each still knows which line it is and which lines are its own |
| Continuing on a line after a switch | `—` | A line that became the default branch calls `end` on itself, hands its work to another line, keeps waking its opener with idle notices, or cannot open a line while the root could |
| Opening lines after a switch | `timeline-branches/switch-then-open-lines` | The new default branch does both independent additions itself instead of opening a line for one of them, or the former default branch takes the new work |
| The root opens while a line holds the role | `timeline-branches/root-opens-after-switch` | The user asks the root for a line after another line became the default branch: the root must open it, the new line must start from the root's history, and it must report to the root |
| The user's line takes the role | `timeline-branches/user-line-becomes-default` | A line the user opened becomes the default branch while a tool line held it: the user's line must carry its own work forward and not end itself, and the former default tool line must answer the input addressed to it and stay within its remaining scope |
| Ping-pong | `timeline-branches/ping-pong` | Steer exchanges that do not move the work forward |
| Asking the user | `timeline-branches/asking-the-user` | A line asks the user directly when it should, and does not route through `main` |

A row whose scenario directory is `—` has no scenario in the set.

### 12.4 Texts and their classes

Every model-facing text of the branch mechanism, with its class ([model-facing prompts §4](model-prompts.md#4-protocol-and-policy)). [§12.1](#121-context-mapping) holds the exact text; a sentence is named here by its opening words, and a mixed text is marked sentence by sentence.

**Protocol.**

- **Marker names and attributes.** `dim-branch-line` with `branch`, `parent`, `source` and, on a status note, `default` (`true` or `false`), keyed `branch:line`; `dim-branch-lines` with `branch`, keyed `branch:roster`; `dim-branch-parent` with `branch`, `head` and `after`, keyed `branch:parent`; `dim-branch-message` with `from`, `to` and, on a final message, `final`; `dim-branch-idle` with `branch` and `status` (`completed`, `failed` or `cancelled`); `dim-branch-ended` with `branch`. The modes are the InputQueue's: Queue for the identity, Append for the roster, the parent block and the status note, Steer for the notices and for a final message.
- **The identity blocks' factual sentences.** In a tool line, "You are line C of this session" through "are P's, not yours.", and the label "Your purpose, as P wrote it:"; in a line the user opened and in a plugin line, the block's whole sentence, whose `source` attribute names which of the two it is. A purpose after the label is the creator's own words, carried as content.
- **The roster's factual sentences.** "Lines you opened that are still open. Each is you …" and "All lines you opened have ended."; each listed line carries its id and the first line of its purpose.
- **The parent block's factual sentences.** "Line P has new activity: it was at H when this was written, and you have read it to A.", the action and field names in "Read what is new with the branch tool (action read, branch \"P\")", and the rule "with no after, the read continues from A."
- **The notices' factual sentences.** "Line C's turn ended (<status>) without a message to you, and C is waiting." and "Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages".
- **The branch tool's actions and modes.** The actions `create`, `message`, `read` and `end` and what each one does; `mode` `append` or `steer` and what it does to the receiver; the fields each action takes, with `action` required and nothing else accepted; the `read` target rule and its cursor rule ("With no after, a read continues after your last read of that line"); and the sentences that state a guarantee: "Returns the line's id", "Pass after to read from another point", "It is your last action" and "A line messages and reads the line it was cut from and the lines it opened. The end action ends the calling line and every line below it."
- **The results and refusals.** `Line created: C`, `Sent to C.`, the page `{items, nextCursor, done}`, `Line C is ending; your final message was sent to P.` and `Line C ends with this message.`; the codes `branch_not_opener`, `branch_limit_reached`, `branch_not_kin`, `branch_not_tool_line`, `branch_ended` and `branch_cannot_end`, with the text each one returns, and `branch_already_default`, `branch_target_protected` and `branch_not_selectable`, which the switch method answers at `session.branch_set_default`; `after A is past line T's head H; a read starts at or below H`, the error result a read returns for an `after` past the target's head, with no code; and `Invalid branch arguments: …`.
- **The status notes' factual sentences.** A note to the new default branch states its id, its parent, its source and its purpose (from the birth block it repeats), the line it was switched from, and the rules that hold while the block is the line's latest one: it answers the user here, it may open lines with `create`, it sends no automatic idle notice to the line that opened it, and it cannot `end` itself. A note to the former default branch states the move that took the role from this line, naming the line it moved to, that this line stays open and keeps coordinating the lines it opened, that it can no longer open lines, that only a line opened with `create` can end itself and not while it or a line below it is the default branch, and that the rest of its birth identity's rules hold again. Neither says whether any other line is the default branch now (§6.2), the root gets no note, and the rule a note states about whether `end` applies is the tree's own, applied whenever `end` is called (§10).
- **The guide entries.** An entry states what its marker is, whose words or which facts it carries, and that it is state or information and never a request. The `<dim-branch-line>` entry states that a later block of that name replaces it, that a block carrying `default` is the note a move writes, and that it belongs to TimelineTree, which writes every identity block, so it is in force in every execution profile. The `<dim-branch-parent>` entry belongs to TimelineTree with it and registers at `session` scope, as Goal's does ([Goal §A.5.1](plugins/Goal.md#a51-protocol-and-policy)), because a line's System has to equal its parent's (§7.3): it is in force on `main` too, which never receives the block. The entries of `<dim-branch-lines>`, `<dim-branch-idle>` and `<dim-branch-ended>` belong to BranchLine, which writes the roster and both notices; the `<dim-branch-message>` entry registers with them, and BranchLine is loaded exactly where BranchTool, which writes a message, is. The roster and message entries stay on every line.

**Policy.**

- **The coordination text for an opener.** The `branch` tool description's guidance, the same text on every line and therefore for both openers: the opening paragraph on what a line is and what coordinating means for a line that may open lines, the `create` rule for when to open a line and what the brief carries, the `message` rule for choosing a mode and for sending a decision instead of a progress report, and the `read` rule for reading a line's work instead of waiting for a report. [`BranchToolConfig.description`](../../crates/dim-agent-capabilities/src/timeline_tree.rs) replaces the description and nothing else. It covers [§12.2](#122-items-for-the-prompt-layer-429)'s first item, how an opener briefs a line and assigns its files or area, in the `create` rule, and how it answers steers and does not redo a line's work, in the opening paragraph's coordination sentence. The roster's closing sentence, "They message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you.", is coordination text for an opener too, and its "end the wait when your lines have nothing left to tell you" covers ending the opener's own wait; [`BranchToolConfig.roster_closing`](../../crates/dim-agent-capabilities/src/timeline_tree.rs) replaces it and nothing else.
- **The line's working rules.** The five bullets of "How you work with P:", on the purpose, the reads and messages — including "Do not send P a progress report it can read" and "do not page through P's whole stream" — the workspace, the user and the end, including "Read P once more before you end if the parent block says P has moved", apart from the clauses that name a mechanism: the opening rule, the action and mode names, and "the file was modified after it was last read", the failure `edit` and `write` report, are protocol, as above. It has no factory member yet, and gets one in the change that first evaluates a candidate for it.
- **The parent block's guidance.** "Read it when what P did changes what you do, and before your final message; do not page through all of P's work." It has no factory member yet, and gets one in the change that first evaluates a candidate for it. The rest of the block is protocol, above.
- **The notices' guidance.** "Read it if you need its state; message it with mode \"steer\" to give it more work, or leave it." and "read it if you need what it did." It has no factory member yet, and gets one in the change that first evaluates a candidate for it.
- **The status notes' guidance.** Seven sentences are policy, each behind its factory member: on the new default branch, "The user switched here to continue directly from this line's context and approach, so the user's request takes precedence over the purpose above, which still says what this line is for.", "Carry this line's main work forward yourself; open a new line only when the four conditions for opening one hold again, not to hand off your current task." and "Avoid redoing work assigned to lines opened before your cut."; on a new default branch the user opened, "Carry this line's work forward yourself; open a new line only when the four conditions for opening one hold again."; on the former default branch, "Keep coordinating any open lines you already opened, and answer messages or user inputs addressed to this line.", "You share the workspace with line N; do not edit files outside your remaining scope unless asked." and, on a tool line, "If end is refused when your purpose is done, message P with what you found, did, changed and left undone and keep working." [`BranchToolConfig.default_note`](../../crates/dim-agent-capabilities/src/timeline_tree.rs), [`BranchToolConfig.user_default_note`](../../crates/dim-agent-capabilities/src/timeline_tree.rs) and [`BranchToolConfig.former_default_note`](../../crates/dim-agent-capabilities/src/timeline_tree.rs) replace them and nothing else. The product sets none of the three, so a shipped note carries no `How you work here:` block: the block is written only when a member is configured. Everything else in the notes is protocol: the restated identity, whether the line may open a line, that it sends no idle notice, whether it may `end` and why, the line it was switched from or the line the role moved to, that a former default branch stays open and keeps its lines, and that only a line opened with `create` can end itself and not while it or a line below it is the default branch. The opening rule is protocol rather than policy because it is the tree's own answer to whether `create` would succeed, and a note that hedged it would tell a line less than the tool call tells it.

A candidate for a policy text is put in place at that text's factory member ([model-facing prompts §8.1](model-prompts.md#81-the-rule)) and keeps the protocol sentences of the text it replaces byte-identical.

The coordination text and the status notes are judged together, as one family, in the change that first puts a candidate for either in place ([model-facing prompts §8](model-prompts.md#8-judging-a-change)): the tool description at `BranchToolConfig.description`, the status notes at `BranchToolConfig.default_note`, `BranchToolConfig.user_default_note` and `BranchToolConfig.former_default_note`, and the roster's closing at `BranchToolConfig.roster_closing`. A later change to one of them re-runs the family, because a text that reads well alone can still lose a comparison another one wins. The opening text is judged on the should-open scenarios of §12.3 and, for this design, on `root-opens-after-switch`: a guard that let only the default branch open a line would make the root answer the request instead of opening one, and that scenario is where the difference shows.

## 13. Interactions

| Feature | Interaction |
| --- | --- |
| Goal | Goal's own rules are unchanged. A line's opening input is a sourced input under A5 (§7.4), and Goal's conflict table records it. A new line clears the inherited goal as before. A line reads the line it was cut from, so the evaluator reads the worker's work after the turn it was cut at; Goal's fold does not change, because the evaluator reads through a tool result on its own line. Goal's code changes only through the Pursuit amendment: recognition from `info(own id).source`, ending its evaluator, and the `branch/ended` key ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) |
| Pursuit | The evaluator gets the plugin line's identity block and can read the worker's work after its cut (§6); what it decides is the evaluation, judged as before ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)). It is a plugin line, so it is never on a roster and never becomes the default branch |
| Fork, delete | Fork copies the root only, as before, so a forked Session's default branch is the root ([fork §6.2](appserver/session-deletion-and-fork.md#62-why-main-only)). Delete is unchanged. Ended lines follow their facts (§10) |
| Session close, idle unload | Not an end. Lines restore as open |
| Interrupt | A turn interrupt, or a stop on a line, ends a turn and never a line. An interrupt on the default branch does not touch the lines around it |
| Session model, profile, prompt, language | The model, the profile and `prompt.refresh` act on the default branch, and a line's model comes from the prefix it inherited, so the default branch's earlier turns keep the configuration they were run under (§22). Language is Session-wide and broadcast to every loaded line |
| Scheduler | A newly created job with no `branch_id` recorded takes the default branch at creation time. An existing job keeps the `branch_id` it recorded, so a move of the default branch never redirects a job that was already scheduled |
| Collaboration | Unchanged. Members are Sessions; whether a member's Session mounts `branch` is up to its profile's tool list |

## 14. Clients

**TUI.** Lines get their own panel, view and controls. Opening a line or a fork from a point of the shown Timeline, and where each line was cut, are in [Timeline-first TUI](timeline-ui.md):

- `/lines` lists every line of the Session, each indented under the line that opened it, with its state: working, waiting, ended itself, ended by an operation, ended with X, or creation failed. The default branch is marked 主线 and the root 起点.
- Enter shows the selected line's conversation. The composer submits to the line shown, and the composer's bottom border names it, with the first eight characters of its id.
- `x`, then `y`, ends the selected line, refused on the root, on the default branch and on every ancestor of the default branch.
- `p` chooses the selected line as the default branch ("continue from here"), refused on a plugin line, on a line created through a client before the upgrade, and on the line that is already the default branch, the root included when it is. The panel states that the old default branch stays open and that the two lines swap the right to open one. The root keeps its own right in either direction: it opens a line whether or not it holds the role, so a move to it takes nothing from it and the panel does not say the two lines exchange it when either line is the root. What the panel describes is the model tool's rule, which these two lines alone satisfy: `session.branch_create` is the user's own entry and still accepts any valid parent, so the panel never tells the user that a line's inability to call the tool blocks their request.
- Questions and approvals carry the line's label, so two lines asking for the same thing are told apart (`(branch NAME)` on a question, `Permission on branch NAME` on an approval). A pending approval stays on the line that asked, whichever line the user has since made the default branch.
- A message between `main` and a line reads as sent on the sender (its `branch` tool call) and as received on the receiver (`◂ from C`).
- Switching the shown line starts another transcript under a boundary row (`line C`), as a session switch does, because terminal scrollback is append-only. Discovery retains task inventory and branch-history state, not a full transcript for every line. The first open, through `/lines` or a side view, retains that line's [bounded transcript window](clients.md#opening-a-conversation-with-a-bounded-window), including when discovery previously failed. Owner-state reads recover actionable work outside the window. Selecting a line separately requests its display details below. Later opens reuse the retained transcript, which continues folding live events even while another line is shown.

The panel is separate from the tasks panel, which lists background commands, members and Workflow runs: its `x` stops a task, and an end is permanent, so the two do not share keys.

```
┌ session 3f2a · 主线 b-7c ────────────────────────────────────────┐
│ … b-7c's conversation …                                          │
│ ◂ from main (steer)  the leak is in cache.rs                    │
│ ◂ from b-02 (steer), final  docs pass done                      │
│                                                                 │
│ ╭─────────────────────────────────────────────────────────────╮ │
│ │ › ask anything…                                             │ │
│ ╰───────────── ~/repo/dim (main) ────────────────────────────╯ │
└─────────────────────────────────────────────────────────────────┘

┌ lines ──────────────────────────────────────────────────────────┐
│ ○ main             起点                                         │
│   b-7c  parser race hypothesis      working · 主线 · shown      │
│   b-91  review the cache change     waiting                     │
│     b-c3  the guard in cache.rs     working                     │
│   b-02  docs pass                   ended itself                │
└ Enter show · p continue from here · x end it · Esc close ───────┘
```

**The branch directory.** `session.branches {sessionId}` (v3, listed in `hello.features` with the other
Session methods under the same admission rules as `session.history`) answers one entry per line: its identity
(`branchId`, with `parentBranchId`, `branchPoint` and `createEvent` when it has them), its `source` as
`TimelineInfo.source` carries it, `state` (`open`, `ending` or `ended`), `head`, and, for an ending or ended
line, `endEvent`. The reply also carries one top-level `defaultBranchId`, rather than a flag on each row: a
top-level field cannot express the illegal states of zero or two default branches, and a client that does not
know the field ignores it. `main` comes first and the rest follow in `branchId` order. The reply is not paged: it
grows with the number of lines, which a Session bounds by its own use, not with history. `TimelineTree` owns the
answer (`directory()`); `branches()` lists the lines that have not ended (open and ending), for the callers that
hold a live line. A client does not need a separate field for who may open a line: the root and `defaultBranchId`
are both in this reply, which is the whole rule (§4).

`session.branch_set_default {sessionId, branchId}` (v3, listed in `hello.features` with the other Session methods)
chooses the default branch. The target is an open conversation line: a tool line, a line the user opened, or the
root, which is a valid target whenever it is not already the default branch, so the user can move back to where the
Session started. The method answers three refusals and writes nothing for any of them:

| Case | Code |
| --- | --- |
| The target is already the default branch, whatever line it is | `branch_already_default` |
| The target is a plugin line, or a line created through a client before the upgrade (§4) | `branch_not_selectable` |
| The target or any ancestor of it is ending or ended, so an end already on its way to it wins | `branch_target_protected` |

Otherwise it writes `branch/default` on the root, writes the status notes (§12.1) — one to every participant of the
move that is not the root, and none to the root, so a move back to the root still writes the note the line losing the
role needs — answers the new `defaultBranchId`, and touches no other line. This is the "continue from here" action; it writes no Session-level
configuration.

The move and an end take the same tree lock and admit in one order (§10): a target whose chain is already ending is
refused, and a line whose descendant is the default branch is refused as an end target. So of two operations that
conflict, exactly one is admitted, and each reads the state the other would have written.

**The default branch on older connections.** The move is reachable through RPC now, but the clients that follow it
come later, so every field and behaviour an existing client relies on keeps its old meaning and the new meaning
arrives through new fields. A v1 connection's event projection carries the root alone (`connection/subscription.rs`
`project` and the v1 branch watch), so on that connection an omitted `branchId` keeps naming the root for every call
this section routes a default for: `session.submit`, `session.stop` and `session.idle` (the three a v1 client may send
unnamed; every other routed call is admitted from v3 on, whose projection carries every branch). A v1 client therefore
still sees the turn its input runs after a newer connection has moved the default. v2 and later connections carry every
branch, must name a line where v1 could not, and a v3 connection's unnamed calls follow the default branch. Nothing
else an older client reads changes meaning: `SessionInfo.sequence` on the `session.attach` reply stays the root's
sequence — the cutoff the CLI keeps as the `main` window's `through` and as the root model-update watermark — and the
line the conversation is on arrives beside it as `defaultBranch {branchId, sequence}`. The cursor belongs to that reply
alone: a `session.summaries` item is a directory row, so it carries no `defaultBranch`, and a client reads the line
after it attaches rather than from a list. A client that predates the field ignores it; one that follows the move
resumes that line from it. The same rule holds the catalog row's
`sequence`: it is the root's in both projections, the loaded overlay and the stored query, so loading or unloading a
Session never changes it, while `running` and `updatedAt` follow the default branch. A persisted move refreshes
`updatedAt` in its own transaction from the new default's latest activity, and the switch itself counts as none, so an
idle move never leaves the row recent by the line the Session left.

The reply's `model`, `modelPreferences` and `capabilitySnapshot` stay the root's adopted configuration, the watermark a
Session on its root has always read. A client that follows the move pairs the configuration with the cursor instead of
mixing the two: it takes the configuration of the line the cursor names through
`session.model.get {sessionId, branchId, through}`, so a Session whose conversation is on a line shows the model that
line adopted at or below its cursor, and the reply's record seeds the root alone. The read answers the line's own
configuration with its own committed cutoff, so it works when a display transcript omits `context/configured` and a
bounded window begins after the adoption; that cutoff is the watermark a later configuration must pass. A server that
cannot answer the read does not name that line's configuration, and its attach reply is the root's, so no client shows
the root's model for a line. A reply that
carries no `defaultBranch` names the root, because a server without the field cannot move the default branch, so
its record is the Session's one model and every client keeps using it.

The directory read performs no history I/O. Its display fields are null or absent.
`session.branch.get {sessionId, branchId}` (v3, separately listed in
`hello.features`) reads one line's directory record and display fields:
`purpose` (the trimmed first prompt line, null for the root), creation `source`, and
`end` (the existing `BranchEndData`, null without an available end fact).
`cutMessages` remains an optional compatibility field and is not calculated.
The call reads only the identified creation and end facts. A read failure is
reported on this detail call and does not prevent directory listing; an unknown
branch is an error. This read writes no facts and does not reopen an ended line. An ended line inherited by a Session fork can
have no own history; its fixed directory end identity remains even when the
`end` detail is unavailable.

`session.model.get {sessionId, branchId, through?}` (v3, listed in `hello.features` with the
other Session methods under `session.attach`'s admission) reads one line's adopted configuration at
that line's own cutoff: `{branchId, through, configuration: {model: {providerId, modelId},
catalogProviderId?, capabilitySnapshot?, modelPreferences?} | null}`. The returned `through` is always the requested
line's committed cutoff, never the sequence of an ancestor's adoption, so a client applies only
that line's `Configuration` events whose `sequence > through` and refreshes by calling again without
`through`. An open line waits for the Dispatcher to reach the committed head, so an adoption
committed just before the call is included rather than missed, and it reports the cutoff of the
state it answers from: that is the captured head while the owner is still there, and the last
applied source fact's sequence when the owner has already applied a later one, so reading at the
returned cutoff always answers the returned configuration. A line whose scope is closing answers
at its published head instead of its owner: a fact whose publication is still pending is not visible
to any read, so the line does not answer past it, and the read never replays sources or writes a
recovery checkpoint while the line closes. An ended line reads at
its fixed head through the ancestor chain and is not reopened; an explicit `through` reads the same
chain at that cutoff, and a value past the line's head is refused as a future sequence. An unknown
line is an error. Old servers do not advertise the method: a client then uses the attach reply for
the root alone and does not present the root's configuration as another line's.

Three rules about `head` decide what a client may do with it:

- An `ended` line's `head` and `endEvent` are fixed. The client passes `head` as `through` to `session.history` to
  read the line, and `endEvent` names the line's own `branch/end`, including when an ancestor's end cascaded to it.
- An `ending` line's `endEvent` is its committed `branch/end`, while its `head` is still live: the line's ended row,
  and with it `branch/ended`, is still ahead. A line reads `ending` from the commit of its own `branch/end` — a tool
  end, a user end or a cascade end — until its ended row is recorded, and input is refused throughout. A
  tool-sourced end written inside a running turn stays `ending` until that turn ends and the close completes. A
  restart does not keep this window: a line whose `branch/end` is committed but whose ended row was never written is
  restored as `ended` (§10).
- An `open` line's `head` is live. Restore appends `branch/resume` and the interruption facts to every open line, so
  `head` moves across a restart; a client re-reads the directory after it reconnects instead of caching a head.

**Web** centres the default branch and follows the other lines in a right-side detail panel: a Lines tree, one tab per opened line, and line-scoped submit, stop, approvals and end ([Web workspace §4.1](web-workspace.md#41-lines-and-branch)). Moving the default branch is the same action as in the TUI, through `session.branch_set_default`. The Lines tree marks the
root and the default branch, which are the two lines that can open a line; every other line's create is refused, and the
refusal is shown where the action was raised. The commands the composer offers act on the line currently centred, not on the root: `/goal`, `/compact`, `/usage` and `/effort` omit the branch or name the line shown, while `/fork` still copies only the root ([Web workspace](web-workspace.md)). A session list shows a Session's running state and its most recent activity from its default branch, so a Session whose conversation is on a line reads as busy or recent by that line, not by its root. Desktop gets the protocol: `session.branch_set_default`, and the new event in v3 history.

## 15. Changes

| Module | Change | Why |
| --- | --- | --- |
| [Design rules §2](../architecture/rust-agent/principles.md#2-作用域的含义) | Rewritten: lines of one subject, shared facts, parent and child as the only communication and reading relation, and the shared interaction surface. Relations row: "only between parent and child", reading the parent's committed events through a tool result. Session row: adds "end" | The current text, "equal lines, the relation only records the source", has no communication relation |
| TimelineTree | The identity block in `first_input` for every source. `TreeCommands.end(id, source)` and `TreeCommands.page(id, after, limit)`. `TimelineInfo.source`, filled from the parent's `branch/create` (§5). The end flow and its guards, refusing input once a line is ending, `branch/ended`, the restore rule, reads of ended lines, and the default branch: `branch/default` on the root, its restore read, and the fact the write makes. Its `directory()` reports an open line, an ending line (its own `branch/end` committed, from a tool, a user or a cascade, until its ended row is written) and an ended line, with the ending line's live head, and the current `defaultBranchId`. Its node publishes the sequence of the line's latest turn end to the line's children (§7.1.1). It also delivers the final message a tool end recorded: a tool end's `branch/end` carries it (§10, §16), so restore writes it on the parent under the id derived from that command when the parent holds no input with it, and writes nothing when it does | It owns the line lifecycle, the opening input and the default branch's one authoritative fact |
| TreeTimeline | Consumes `branch/end`, plus the line's next turn end for an end issued by a tool call, and routes them to TimelineTree. It reports every `turn/end` to TimelineTree, which publishes it for the line's children | It routes `branch/create` the same way |
| BranchTool | The action-based tool, plus the new BranchLine branch part. `read` and `message` reach the line the caller was cut from and the tool lines it opened, with one cursor rule. `create` admits the root and the current default branch and refuses every other line with `branch_not_opener`, then checks the Session's cap and refuses with `branch_limit_reached` | The tool's actions and one line's view of its lines |
| Session facade | `Session::end_branch(id, source)`, `Session::default_branch(id)` and `Session::set_default_branch(id, source)`. An ended line's head is `Session::sequence`; its history is read at that fixed head through `TreeCommands.page` | §17 |
| App Server | `session.branch_end` (v3, a user source with a minted `operation_id`, listed in `hello.features` with the other Session methods when `session_execution` is on and the connection's entry is not bare or the daemon profile is not protected). `session.branch_set_default` (v3, the same admission). `session.history` on an ended line returns immediately, at its stored head. `session.branches` reads the tree's directory and carries `defaultBranchId` (§14). Every Session-level default target that today names `main` reads the default branch instead: `session.submit`, `session.stop`, `session.idle`, `session.goal.*` and `shell.run`, and a newly created scheduled job. `session.model.update`, `session.agent.update` and `session.prompt.refresh` act on the default branch rather than refusing every line but the root. `approval.respond` keeps the `branchId` its notice carried, so a pending request stays on the line that asked for it. On a connection whose projection carries the root alone (v1) an omitted `branchId` keeps reaching the root; v2 and later may name a line, and a v3 connection's unnamed calls follow the default branch. `session.attach` keeps `sequence` as the root's and adds `defaultBranch {branchId, sequence}`; a catalog row's `sequence` stays the root's in both projections while `running` and the activity follow the default branch, and a persisted move refreshes that activity in its own transaction. `session.branches` reads the default and the directory under one tree snapshot. Collaboration stores the member branch a request's input was delivered on and reads it for the idle wait, settlement and every stop; a read page reports its branch, and a continuation carries it or is refused. PromptSuggestion binds its cached turn, offer and generation to a line and withdraws them on a move. EventHub answers admission, source ownership and the type before it resolves an omitted target branch. `ProductEvent::new` carries the new kind in v3 and leaves it out of v1 and v2 | An entry and three reads, no business logic |
| dim-agent-types | `branch/end` and `branch/ended` in `EventKind` and the v1 exclusion list, then `branch/default`, registered in the contract tests. `BranchSource` gains a kind for a line the user opened, distinct from a plugin's. The frozen fixtures are unchanged | Core facts (§16) |
| Storage | `FORMAT` 52 → 53 as a marker-only upgrade `(52, &[])`: no table scan and no data migration, so an older daemon refuses the store instead of failing on its first decode. `SqliteTimelineStore`'s writer thread caches each Session's default branch for the catalog's recent activity, initialised by the same indexed point query that restore uses | A new event kind and a new `BranchSource` variant are rows a 52 daemon cannot decode |
| Goal (Pursuit amendment) | Recognizes its evaluator from the line's source and ends it ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)) | Goal owns its evaluator's protocol |
| Branch TypeScript binding | Regenerated `npm/branch-services.d.ts`: TreeCommands gains `end` and `page`, and `TimelineInfo` gains `source`. The `treeCommands` row of the [binding table](branch-typescript.md) lists `end` | TreeCommands is an exported service |
| CLI TUI | §14 | Users follow, talk to, move to and end lines |
| Plugin pages | TimelineTree.md and BranchTool.md, updated with the implementation (`xtask check`) | Pages follow the code |

New public content:

- three events: `branch/end`, `branch/ended` and `branch/default`;
- one RPC method, `session.branch_set_default`;
- one TreeCommands read and one write, `default_branch()` and `set_default_branch(id, source)`;
- `defaultBranchId` on the directory reply;
- `defaultBranch {branchId, sequence}` on the `session.attach` reply, beside the root's `sequence`, and on no `session.summaries` item;
- the `branchId` a collaboration read page reports and accepts for its continuation;
- the `branch_changed` reason a PromptSuggestion clearing notice and refusal carry;
- a `BranchSource` kind for a line the user opened;
- the roster's key, `branch:roster`, announced to the model through the `<dim-branch-lines>` guide entry;
- two refusal codes, `branch_not_opener` and `branch_limit_reached`;
- one branch-qualified read, `session.model.get`, which answers a line's adopted configuration at that line's own committed cutoff.

BranchLine's service is read only by the tool, inside the same plugin. The turn-end signal is crate-internal, beside `TreeCommands.page` and `head`: it is not exported, and the TypeScript binding does not change.

## 16. Format

`branch/end` and `branch/ended` are core events in the `branch` domain, and `branch/default` belongs to the same domain:

| Event | Data |
| --- | --- |
| `branch/end` | `{source, ancestor?, final?}`. `source` is a `BranchSource` (`{kind: "tool", turn_id, request_id, call_id}`, `{kind: "user", operation_id}` or `{kind: "host", operation_id}`); `ancestor` is a line id; `final` is the message text the parent receives, present only on a tool end, because only a tool end owes one and restore delivers a missing one from it (§10) |
| `branch/ended` | `{branch, endEvent, source, ancestor?}`. `endEvent` is the sequence of `branch/end` on the ended line; `source` and `ancestor` are copied from it |
| `branch/default` | `{branch, from?, source}`. Written on the root only. `branch` is the line the Session's conversation is on; `from` is the line it was moved from, absent when that line was the root; `source` is the operation that moved it. The newest such event on the root names the default branch, unless a later `session/forked` on that line voids it. `from` is what a reconciling load reads to write the note a stopped move left out (§5) |

The format decision for them is recorded in the storage contract: [#427 v20 decision](appserver/native-storage-format.md#427-v20-格式决定). `branch/default` is a new event kind and `BranchSource` gains a variant, so the store's `FORMAT` rises with them (§15).

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
  - a command `branch/end {source, ancestor?, final?}` on the line and a result `branch/ended {branch, endEvent, source, ancestor?}` on its parent;
  - `TreeCommands.end(id, source)`, `TreeCommands.end(id, source, final)` for a tool end, and `Session::end_branch(id, source)`, which check only the target; the final message is delivered after the command commits (§10);
  - `TimelineInfo.source`, filled from the parent's `branch/create`;
  - no new input once `branch/end` is committed;
  - descendants end first;
  - restore treats an ended line as inactive and completes a `branch/end` that has no `branch/ended`;
  - an ended line's committed history stays readable at its fixed head, through `TreeCommands.page`.
- **Effect on sequences, batches, interrupt and reload.**
  - Each event is its own batch: `branch/end` on the line, and `branch/ended` on the parent after the close. A tool end's final message is a second event on the parent, after `branch/end` and before the close, under an id derived from the command.
  - An end with a `host` or `user` source, or because of an ancestor, interrupts the line through `Branch::close()`.
  - On reload, the restore rule above applies. A Session publishes after its restore, so a part that ends a line at load, such as Goal's reconcile ([Pursuit §13](plugins/Pursuit.md#13-ending-the-evaluator-with-427)), waits for `SessionLifecycle::activated()` and then finds every line the directory holds published.
  - The Timeline gains the line's own admission check: `Timeline::input` becomes `input_checked(data, admit)`, which runs `admit` under the commit lock, so an input that commits after `branch/end` is refused with `branch_ended` instead of racing the close. The Dispatcher and batching are unchanged.

## 18. Security

- Lines share the Session's permission policy and Approval. A line is not isolation ([ADR 003](../adr/003-approval-scope.md)).
- Only TimelineTree and BranchLine write the markers. Other producers that write blocks (plugins, through `TreeCommands.input`) are trusted in-process parts. RPC writes no blocks.
- `session.branch_end` and `session.branch_set_default` are trusted user entries, as `session.branch_create` is. They need no approval, because they are not agent actions.
- Any trusted in-process part can end any open line the end guards allow, just as it can write input to any line. Approval governs the tool's `end` like any other call.
- The default branch confers no authority. Choosing it changes which line default targets resolve to; it does not widen a line's Permission, Approval or file roots ([ADR 003](../adr/003-approval-scope.md)).
- Diagnostics record ids, sequences and the source kind, never message or purpose text. A tool end's final message is not a diagnostic: it is the one text `branch/end` stores, and it is the same text the parent receives as a steer.

## 19. Rule check

Rules: [design rules §1, §2 and §4](../architecture/rust-agent/principles.md), [ADR 002](../adr/002-plugin-scope.md) D1–D5, and [ADR 003](../adr/003-approval-scope.md).

| Rule | How this design complies |
| --- | --- |
| §1 Capabilities, App Server boundary | Capabilities: TreeTimeline (existing) and BranchLine (new). TimelineTree and BranchTool's Session part are ordinary parts. App Server gains one entry and one read path, with no business code |
| §2 Scopes | The line lifecycle stays the Session's (TimelineTree). One line's view of its lines is branch state (BranchLine). Lines influence each other only by writing input and learn about each other only by reading, now only between parent and child (§2 rewritten). Capabilities affecting a branch are not bound by that rule (§4) |
| §4.1 Command events | An end is `branch/end`. TimelineTree decides and writes `branch/ended`. A message is input whose receiver's InputQueue and Loop decide. An end of an ended line, or a refused message, writes nothing, and the reply says why |
| §4.2 No recall after handoff | A committed message belongs to the receiver. A `branch/end` is not withdrawn |
| §4.3 Before callbacks | None |
| §4.4 No business meaning in core | Dispatcher, `before` and batches are unchanged; the Timeline's admission check is generic (§17 R1). The identity text is TimelineTree's own domain, the lines of a Session. A tool line's protocol is written where `first_input` already writes tool-specific text, in the module that owns the tool |
| §4.5 Batches | `branch/end` stands alone, carrying a tool end's final message. The message itself is one later `input/message` on the parent, under an id derived from that end, so the two are ordered and are not one batch, and restore completes the pair (§10). `branch/ended` comes last, after the close. The roster and the ended notice are one input. `branch/default` on the root stands alone, and each status note is one Append after it |
| §4.6 Context | Identity, roster, messages, notices and the status note are adopted inputs. `read` returns a tool result. Nothing else enters the context |
| §4.7 Public events and services | BranchLine folds its own line's public events. The tool reads BranchLine (same plugin) and TreeCommands. `read` returns committed events as a tool result, the way Collaboration's `read` does, and no part folds another line's facts. Goal classifies the opening input by its own published A5 convention (§7.4). BranchLine and Goal read `TimelineInfo.source` through TreeCommands, a public service |
| §4.8 Infrastructure | The facade and format changes are R1 (§17). They come from the line lifecycle itself and close within the runtime. The default branch's fact, its read and its write are the same kind of infrastructure: one event, one `TreeCommands` read and one write, no business rule in a binding |
| §4.9 Only facts later decisions use | `branch/end` decides the close and the restore, and a tool end's final message rides on it because restore decides from it whether a delivery is still owed, and what to deliver; no other source carries one, and diagnostics record no message text (§18). `branch/ended` decides restore inactivity and the roster. `branch/default` decides which line the Session's defaults resolve to and which lines may open one, both of which later calls read, and a load that completes a stopped move reconciles its note from it instead of taking a second restore read; it carries the line moved to, the line moved from when that was not the root, and the operation, nothing else. No UI-only event and no presentation-only field |
| ADR 002 D1 Scope | As in §5 |
| D2 No wiring modules | BranchTool's feature registers BranchLine itself |
| D3 Scope dependencies | BranchLine depends outward on the Session's TreeCommands. No core part depends on it |
| D4 Configuration | None |
| D5 Language-neutral contracts | Events, the RPC method, markers and the tool schema are data. TreeCommands' new read and write are exported and typed for TypeScript |
| ADR 003 Approval | The tool keeps risk `session`, and Approval admits every call. A user end is not an agent action. The right to open a line is not an authority the default branch confers: it is the rule of the tool, and it widens neither Permission nor Approval |

## 20. Acceptance

These are flows with a fake model, in `branch_flow.rs`, `flows.rs` and the App Server transport flows.

1. `main` creates two lines. The roster under `branch:roster` lists both. Each child's identity, under `branch:line`, leaves the inherited roster alone, and a child that opens a line of its own keeps both. After a compaction on the opener and on a child, the keyed baseline replays each roster and each identity.

1a. A child cut after its parent wrote a roster inherits that block, and its opening input retires it. A child that opens no line of its own writes only `branch:line`, and a compaction of that child replays its identity and no roster: the inherited block was retired at its open, so it never tells the child that it opened its parent's lines or that it may coordinate them. A child cut before its parent wrote a roster gets no such block and is byte for byte what it was.
2. A child sends its opener an Append: it does not wake the opener, which sees it at its next request. A Steer wakes the opener's `wait`, or starts a turn when the opener is idle. A Steer from an opener wakes an idle child.
3. These are refused and write nothing: a sibling message or read, a line and its grandparent, `create` on a line that is neither the root nor the default branch, a message to or from a plugin line, any line's read of a plugin line through the tool, `end` on the root, the default branch or an ancestor of the default branch, and `end` from a plugin line or from a line the user opened. The line a user-opened line was cut from messages and reads it neither (`branch_not_kin`), while that line itself messages and reads the line it was cut from.

3a. A user line is on no roster and under no coordination of the line it was cut from. `session.branch_create` opens `U` from `P`: `P`'s roster does not list `U`, `U` appears in `session.branches` in the Session's directory with its source `{kind: "user"}`, `P`'s `read` and `message` of `U` answer `branch_not_kin`, and a turn `U` ends writes neither an idle notice nor an ended notice to `P`. The user then makes `U` the default branch, and `U` opens a tool line `T`: `U`'s roster lists `T`, `U` messages and reads `T`, and `T`'s notices reach `U`.
4. When a tool child's turn ends without a steer, one idle notice reaches the line that opened it, with the turn's status. None is written when the turn steered its opener, called `end`, left a driving input pending, or when the line is the default branch.
5. A tool line ends itself: `branch/end` with a `tool` source is written carrying the final message, then the parent's steer under the id derived from that source, the close comes at the turn end, then `branch/ended` on the line that opened it and the roster update, with no ended notice. A later message is refused (`branch_ended`), and the opener still reads the ended line's history: the tool's `read` and `session.history` both return it.
6. User end through `session.branch_end`: `branch/end` has a user source with App Server's id, the line's turn is interrupted, and, for a tool line, its opener gets the ended notice as a steer; the same end of a line the user opened writes `branch/ended` on the line it was cut from and no notice there. The root and the default branch cannot be ended, and neither can an ancestor of the default branch. A second end returns `branch_ended`. Submitting to an ended line returns `branch_ended`.
7. Restore: an ended line is not opened, and neither are its descendants. A `branch/end` without a `branch/ended` gets its result written at restart.
8. A Pursuit evaluator gets the plugin line's identity, can read the worker's work after its cut, and the existing Pursuit flows pass unchanged.
9. Two lines edit the same file: the second gets `file_modified_since_read` (existing behavior).
10. v1 and v2 clients do not see the new kinds. v3 history includes them, and `hello.features` lists `session.branch_end` and `session.branch_set_default`.
11. Reading the parent: a line's first read of the line it was cut from starts after the cut; an explicit `after` below the cut is raised to it; an explicit `after` past the target's head is refused through a tool result that names the head, and nothing is stored; a read with no `after` continues from the caller's last `nextCursor`, in both directions, and survives a reload; `main`'s `after: 0` re-reads a line from its start.
12. The parent block: an idle line is told its parent moved without a turn starting, and the block is in the first request of its next turn, ahead of the input that starts it; a running line gets it with its next model request, once; a held block is not written over; a consumed block is followed by a new block for a new head; a line that read to the head is not told; the turn that only opened the line is not activity, while a later tool call or an operator input is; a reload with no movement writes nothing, and a reload with a block due writes one; an ending line and `main` get none; the block replays after a compaction of the line; a nested line's opening input carries the block that names its own parent at the cut, and a line cut from `main` gets none.
13. The parent block changes no Goal decision on an evaluator line: it is an Append input whose first block is not a goal marker and whose key is not a goal key, so the fold's `driving`, `lastOperator`, count and conclusions are unchanged (`goal.test.mjs`).

Goal flows, in the crate's flow tests under `crates/dim-goal/tests` (§7.4):

11. A line opened with `session.branch_create`, whose opening input carries the identity block, behaves exactly as `main` does: `set` on that line, its continuations, and a later operator input that resets the count.
12. A Pursuit evaluator created under the new opening input is recognized and its conclusion is applied. The working line's count is unchanged.
13. The identity block changes nothing in the objective and status fold, the decided set or the continuation list.

Line end and sources:

14. `TreeCommands.end` ends any open line the end guards allow, whoever calls it, for example a plugin part with a `host` source on a tool line, which gives the opener the ended notice. It refuses the root, the current default branch, an ancestor of the default branch, an unknown line, a line that is still opening and an ended line, and writes nothing.
15. Ending a plugin line that has a child ends the child first, with `ancestor` set and `source` copied, then the line. Ending any line whose descendant is the default branch is refused before anything is written.
16. `TreeCommands.list` and `info` return `source` for a new child and for a restored active one, and none for the root. Ended directory entries load their source only in display detail. The stored directory rows are byte-for-byte unchanged.

The branch directory:

17. `session.branches` returns `main` first and the rest in `branchId` order, with each line's identity, source, state and head. A cascade end records the ended child with the sequence of its own `branch/end` as `endEvent`, and `session.history` with `through` set to that child's `head` reads it. A line reads `ending`, with its own committed `branch/end` as `endEvent` and a live head, from that commit — a tool end, a host end or a cascade end — until its ended row is written, and `ended`, with the same event and a fixed head, after it. A tool-sourced end inside a running turn stays `ending` until that turn ends and the close completes.
18. After a restart the directory keeps ids, parents, branch points, create events and states; ended lines keep their `head` and `endEvent`, and an open line's `head` is at or above its old value. A line that ended while its turn was still running restores as `ended`, not `ending`.
19. A fork's directory lists the lines that ended before the fork, with the end the source recorded, and those lines have no events in the fork. A v1 or v2 client has no `session.branches`, and a closed Session answers `session_closed`.

The default branch:

20. A Session with one line is on its root, and its restore performs no query for it. A Session with more lines reads exactly one fact on the root and answers the target of the newest `branch/default`, or the root when the newest of the two kinds is a `session/forked`, or when there is none. A target that has ended, a target that is a plugin line and a target whose creation source is a client's `host` row answer the root.
21. `session.branch_set_default` chooses the default branch: `branch/default` is written on the root, and every participant of the move that is not the root receives exactly one note. A move has two participants, the new default branch and the old one, so `main → B` writes one note (B, `default="true"`), `B → main` writes one note (B, `default="false"`) and `B → C` writes two. The root receives none in either direction: it opens lines and it never ends, whichever line holds the role, so no note could tell it anything. For `main → B → main`, B therefore receives exactly two notes over the two moves, and `main` none, so B's latest block always states the rules it holds now: on the way in that it may open lines, on the way back that it may no longer open lines, and that its idle notice and its `end` rule are its own again. A refusal writes nothing: a plugin line and a client's pre-upgrade line are refused `branch_not_selectable`, an ended or unknown line is refused, a target whose ancestor is still `ending` is refused `branch_target_protected`, and whatever line is already the default branch is refused `branch_already_default`, so the root is refused while it is the default branch and accepted once another line holds the role.
22. A request that omits `branchId` reaches the line the fact names, and work already bound to a line stays there. `session.submit`, `session.stop`, `session.idle`, `session.goal.*` and `shell.run` resolve to the new default branch; a newly created scheduled job records it; a scheduled job that recorded a `branch_id` keeps it. A pending approval stays on the line that asked for it and answers there, before and after a move of the default branch, because `approval.respond` uses the `branchId` its notice carried.
23. `session.model.update`, `session.agent.update` and `session.prompt.refresh` act on the line named, and on the default branch when none is named, instead of refusing a line that is not the root. A line that was a tool line and became the default branch may open a line, sends no idle notice to its opener and is refused `end` on itself with `branch_cannot_end`; once another line holds the role it opens nothing, sends its idle notice again and may end itself. A line the user opened is refused `end` on itself with the same code and a plugin line is refused it too. `create` on a line that is neither the root nor the default branch answers `branch_not_opener`, and the root answers `create` in every state.
24. A fork of a Session whose default branch was not the root answers the root as its default branch, because every `branch/default` it copied is older than its `session/forked`.
24a. A move that stopped between its fact and its notes is completed at load. The daemon stops after `branch/default` for `B → C` commits and before either note is written. After the restart, B receives its `default="false"` note and C its `default="true"` note, each before that line's next model request, and `main` receives none. The note C receives names the line the move started from, B, from the `from` field of the fact. A second restart finds each line's block and the tree in agreement and writes nothing, and the failed move is not retried as a whole: the fact stands, and only the notes it owes are written.
25. Two successive moves leave no stale instruction: after `main → A → B`, the root has received no note, and `A` holds the note that says it is no longer the default branch. `A` is refused `end` while it is an ancestor of `B`, and is admitted `end` once the user moves back to the root, because the guard reads the tree at the call and not a note.
26. The switch and an end are admitted in one order. With `main → A → B` and the end of `A` started, a switch to `B` is refused `branch_target_protected` while `A`'s cascade is still writing its descendants' ends. In the other order, a switch to `B` is admitted and the later end of `A` is refused, because `B` is below it. Either way exactly one of the two operations is admitted and nothing is written for the refusal.
26a. A switch and a tool line's own `end` are admitted in one order, and a refused end writes no final message. The user makes B the default branch while B's turn calls `end` on itself. If the switch reaches the tree lock first, the end answers `branch_cannot_end`, `branch/end` is not written and B's parent receives no final steer: B is still open and the tree says so. If the end reaches it first, `branch/end` is committed with the final message and the switch is refused `branch_target_protected`, because its target is ending and a target that is already on its way to ended cannot become the default branch. Either way exactly one of the two is admitted, and the parent is woken by a line that really ended or not at all.
26b. A tool end's final message survives a stop in either of its two windows. The daemon stops after B's `branch/end` for a tool end commits and before the parent's steer. After the restart, B is `ending` or `ended` and cannot be selected as the default branch, its parent receives the final message exactly once under the submission id derived from `branch/end`'s source, and a second restart writes nothing. A stop before `branch/end` commits writes nothing: B is open, the tree says so, and the parent has not been told anything.
27. Continuing on a line after a switch, end to end: the user opens line `B` from the root, works on it until it uses a tool whose result it needs later, and chooses "continue from here". A message submitted with no `branchId` reaches `B` and its answer uses that earlier tool result, `branch/default` on the root names `B`, and after a restart with no new input the same message still reaches `B`. Choosing the root with the same method returns the Session's conversation to the root, and a later message with no `branchId` reaches the root.
28. An upgrade case: a store written under `FORMAT` 52 whose Session holds a line created by `session.branch_create` opens under 53 unchanged, and that line is refused as a switch target with `branch_not_selectable`, while a line the user opens after the upgrade is accepted. Nothing in the stored rows is rewritten.

Who may open a line, and the cap:

29. The root opens a line while another line is the default branch, as the user asked it to. `main` opens `B`, the user makes `B` the default branch, and `main` then opens `C` at a cut of its own: `C` starts from `main`'s history at that cut, `main` receives `C`'s roster entry and its final message, and `B` receives nothing about it. Asking `B` for the same line would start it from `B`'s history, which does not carry what `main` did after the split, so the case is the guard's reason to exist and not a workaround for it.

30. Continuing on tool children does not fill the cap. `main` opens `B`, the user switches to `B`, `B` opens `C`, the user switches to `C`, and `C` opens `D`. With a cap of two, the path `main → B → C → D` takes no slot beyond the lines that are on it: the default branch and its ancestors are not counted, so an open that would otherwise be refused succeeds, and the count the model is told about excludes them. Each of those opens still spends a slot, because the line it opens is a new child off the protected path and nothing about the opener's placement is inherited by it.

31. A switch that leaves the Session over the cap refuses new opens and closes nothing. The cap is two and `main → A → B` are the only open tool lines. Selecting `B` protects both, so the counted total is zero; switching back to `main` makes `A` and `B` count together, which is two, and the next `create` answers `branch_limit_reached` with a text that says it is the cap. A single line on the counted side of that subtree leaves the Session over its cap, and nothing is closed either way: every line that was open before the switch is open after it, which the directory reports. A switch never counts only its two participants, and the recorded runs state both halves. `BranchCapSwitch-BackToRoot` starts below both nested lines and admits the switch that spends every slot; `BranchCapSwitch-OverCap` adds one line opened meanwhile and leaves the Session over its cap; `BranchEligibility-AtCapBelow` states the same rule for one line in the single-line model.

32. Every line but the two openers is refused `create`, writes nothing, and keeps the tool lines it opened. A former default branch still messages and reads the tool lines it opened, and its own `create` answers `branch_not_opener`. Its coordination cannot end a child on its own either: `end {content}` has no target, so calling it on B ends B itself and every line below it (§10), not C alone. When the user wants C ended, they end it through `session.branch_end`, or B asks C with a steer message to end itself, which is the convention §10 states. A line below an ending ancestor is ended by that cascade (with `ancestor` set, §10), not by a tool call targeting the child.

33. The last slot and a child of the default branch. The cap is two, one open tool line is already off the protected path, and the root is the default branch, so exactly one slot is left. The root opens a line: the new child is a descendant of the default branch, not an ancestor of it, so it counts, and the slot is spent. A second `create` from the root, from the same state of the tree, answers `branch_limit_reached` and writes nothing. Neither open is exempt for being opened from the default branch, and no line is closed to make room. The recorded model run `BranchEligibility-CapBoundary` covers exactly this: `RootInit` with `peers=1`, one open admitted, and no state where a second is.

34. A switch that frees a slot admits the next open. The cap is two, one open tool line is off the protected path, and the root holds the role, so the counted total is one. The user makes that line the default branch, which is the case the review named: the line stops counting, the total becomes zero, and the root's next `create` is admitted because both slots are free. Nothing is closed to free the slot, and the line the user continued on is still open. The recorded model run `BranchEligibility-MoveToChild` covers exactly this.

35. A reservation holds the last slot. The cap is two, and the default branch is a tool line `B` that `main` opened: `B` is on the protected path, so it does not count. One other open tool line `A` is off that path and counts, which leaves one slot. `main` reserves a tool child and its build stays paused before publication, and the caller stops waiting for it: the build goes on and the reservation is kept. `B`'s `create` from the same state of the tree answers `branch_limit_reached` and writes nothing, because the reservation is charged with the open lines. When `main`'s child publishes, the counted total is two, `A` and the new child, not three: the reservation turned into an open line and was not charged a second time. A creation that definitively fails instead releases its reservation, and the slot is free again.

## 21. Ablation review

What is left, and why each piece is needed:

| Piece | Why |
| --- | --- |
| The identity block on every child, under one key | Without it a line forgets which line it is after compaction, or takes its parent's roster as its own |
| The roster under its own key, on every line that opened tool lines | Coordination follows the parent-child relation, and now any line may be an opener, so every opener needs a roster; it lists only the lines the tool opened, because the line a user line was cut from can neither message nor read it; sharing the identity's key would let a roster write retire a non-root line's own identity |
| Messages as `input/message`, with append and steer | The only path between lines. The modes are InputQueue's |
| `read` for an opener | An opener checks a line's work without asking for it |
| The idle notice | Without it an opener waits on a line that stopped silently |
| `branch/end` and `branch/ended` | Without them a line cannot end durably, and restore reopens it |
| The end guards | Ending a line ends every line below it, so the default branch and its chain must be refused or a move would be undone by an ancestor's end |
| `source` on the end (`BranchSource`) | A create and an end record their issuer the same way, so a plugin's end is not recorded as the line's own |
| `TimelineInfo.source` | An opener finds its own lines without parsing prompt text |
| A source kind for a line the user opened | The core must tell a conversation line from a plugin line without parsing an `operation_id`, and a stored `host` row a client wrote must stay distinguishable from one a plugin wrote |
| `branch/default` on the root | One authoritative, non-model fact: the App Server, the store's writer thread and restore all read it without a model or a keyed block, and the root cannot end |
| A status note per non-root participant, and none for the root | Each participant whose rules changed learns it once, and no other line is touched, so no other line's cached prefix moves. On a move back to the root only the line losing the role changed, so only it is told. The root's rights do not change with the role, so a note to it would say nothing |
| The idle-notice exclusion and the `end` refusal for the default branch | Without them a line the user continues on keeps waking its opener on every turn and can end the very line the Session's conversation is on |
| The directory's `endEvent` and its `ending` state | Without `endEvent` a client cannot name a cascaded line's end, and the line's own `branch/end` is one page away. Without `ending` the window between a line's own `branch/end` — a tool end, a user end or a cascade end — and its ended row reads as open, and a client cannot tell a line that takes input from one that already refuses it. With `defaultBranchId` a client also centres and labels the right line |
| The tool opened by the root and by the current default branch, and by no other line | The root keeps the ability it has today, which is what lets the user ask it for a line while another line holds the role; the default branch can grow the tree from the work the user continued on; and every other line stays a leaf, so the openers are two and the coordination each line was given at its creation does not move |
| The cap's counting rule | Counting the protected path would spend slots on lines that cannot be closed, so a user who keeps continuing on tool children would fill the cap with the line they are working on. A new child is off that path, so an open always spends a slot and the cap has one rule rather than one per opener. The rule is read from the tree in both directions of a move, which is what the `DefaultLeavesBelow` fact carries: a line below the one that loses the role counts again, and the cap answers with a refusal rather than a close |
| The cap charged at the reservation | A reservation outlives a caller that stops waiting, so a check over published lines alone lets two calls take the last slot. Charging it once, where the line is reserved under the tree lock, is what makes the count the same before and after a publication |
| The prompt rules on files | The only guard is FileReads; the rest is protocol |

Removed:

- caps on lines, messages, steers or rounds;
- attribution of file changes;
- changes to WorkspaceTools;
- a channel between siblings;
- per-line configuration (system, model, tools);
- a result contract and hand-back;
- a line reading the root;
- a status note to the root, and a fork rule that retires one;
- a guard that ties opening a line to the default branch alone, which cannot serve a user who asks the root for a line while another line holds the role;
- closing a line to make room for a new one when a switch leaves the Session over the cap;
- separate events or concepts for messages (they are InputQueue modes);
- a parent ending its children through the tool;
- a `by` field with fixed values (the existing `BranchSource` records the issuer);
- a category field on plugin lines (`operation_id` already is the opener's tag);
- a runtime check of who ends a line;
- - a note stating whether another line is the default branch (a later move would leave it stale, and `end` reads the tree instead);
- a stored `default_branch_id` column (the fact is one event, and the writer thread caches it).

## 22. Later options

**Per-line narrowing within the Session's limits.** Narrowing within the Session's limits keeps the same subject, so it is compatible with design rules §2. It is not part of this design. When it is built:

- **Fewer tools:** refusal at call time, as `create` on a line that may not open one is refused now. The tool definitions and the cache prefix stay identical.
- **Lower permission:** a per-line policy intersected with the Session's. It needs the per-branch Permission narrowing that [Pursuit §10](plugins/Pursuit.md#10-later-options) already names.
- **Cheaper model:** per-line configuration set at create, before the first input. A different model shares no cache with its parent, so it trades the inherited prefix for cost.

**Nobody else opens a line.** The two openers are the root and the current default branch (§4). A third kind of caller would be a separate decision, not a missing case: any line opening a line is what makes the tree deeper than the two openers' work needs, and it is what would move coordination away from the line that created a line. If the cap's number has to change, it is set by measurement (§12.3) and carried in the implementing change, not guessed here.

**Migrating the pre-upgrade `host` rows.** A line a client created before `FORMAT` 53 keeps its stored `host` row and is not selectable (§4). A change that wants those lines selectable needs a data migration decision of its own, because the stored row cannot tell a client's line from a plugin's without the new kind.

**Model, profile and prompt per line.** The model, the profile and `prompt.refresh` act on the line a request names, and a line's own configuration comes from the prefix it inherited. Language stays Session-wide and broadcast to every loaded line.

### Resume reads

BranchLine mounts at the committed cutoff with no historical event query. It does not restore old turn counters, steering progress or a table of held inputs. A recovery terminal for a turn whose start was not consumed by this generation produces no idle notice. After the Session is published, roster reconciliation uses the currently open child directory and reads a missing purpose at its known creation position. It compares only the last roster block. Parent notification reads the last parent block and the cursor of that parent when needed. Read cursors are not eagerly loaded for other targets. No recovery fact is added.

TimelineTree locates each child's terminal kind and end position by identity, without creation or end body decoding for an already ended child. Only a line that will mount reads its creation source at its known position. It does not page through all parent lifecycle facts. Existing cuts were accepted on creation and are immutable; restoration uses their directory values without rendering the inherited context to validate them again. New cuts retain their creation-time validation.
