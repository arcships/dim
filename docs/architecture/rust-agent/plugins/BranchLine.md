# BranchLine

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Scope: branch. Issue: [#2024](https://github.com/arcships/dim/issues/2024).

This page is the owner contract for one line's view of itself and its lines.
[Current implementation](#current-implementation) describes the code as it is today.

BranchLine is one line's view of itself and of the lines it opened. It folds the line's own committed facts into its role, the tool lines it opened, the state of its current turn and its read cursors, and it writes the blocks a line's neighbourhood reads: the roster, the parent block, the idle notice, the ended notice and the status note a default-line move leaves. TimelineTree owns the directory and the lifecycle; BranchLine owns what one line says about its neighbourhood and keeps it current. A line that opened nothing and is the root writes no block.

## Use and scope: responsibility split

BranchLine owns:

- **Its role and parent.** `main`, `tool`, `host` or `user`, and the line it was cut from, both read from the parent's `branch/create` at the line's opening cut — never parsed from rendered history.
- **The tool lines it opened and their states.** The roster of open tool lines with the first line of each purpose, in opening order, retired when none is open.
- **Its read cursors.** The `nextCursor` of its latest successful read of each target it may read.
- **The parent block.** The head and cursor the line has reported to itself about the line it was cut from, and whether a block is currently held and undelivered.
- **Its turn and notices.** The state of its current turn, and the idle notice it steers into its opener. The roster and the ended notice are written when this line is the opener of a line that ended: the opener's own BranchLine writes them on the opener's own line.
- **The status note.** One keyed block when the line becomes, or stops being, the conversation line, with the text this owner writes after TimelineTree records the transition.
- **The model-facing markers.** `<dim-branch-lines>`, `<dim-branch-idle>` and `<dim-branch-ended>`, and the guide entries of all four markers, until this part unloads. `<dim-branch-message>`'s text shape and guide entry are this owner's; the block itself is written by BranchTool for a message and by TimelineTree for an end's final message.

Callers keep: which lines they message or read (BranchTool's kin rules), what a message says, how a page is rendered, and every decision about the caller's own work.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| BranchTool asks this line's role before deciding what it may message, read or end | No: the role and parent are folded from the line's own facts and cached; re-deriving them in a Session part means folding another owner's state | `view` |
| BranchTool learns whether this line has committed its own end | No: same fold | `view` |
| BranchTool continues a read of a target where the line's last read stopped | Yes: the view answers that target's cursor in the same call | `view` |
| Write the roster, the parent block, the idle notice, the ended notice, the status note | No, but not public: these are this owner's own writes, driven by the line's facts | Internal |
| Tell the opener the line moved: a parent turn end past the cursor | No: it is this owner's fold of its own facts and its own write queue | Internal |
| Tell the opener a line it opened ended | No: only the opener's own BranchLine can write its line, and the ended line's writer is closing | `branch/ended` fold, internal |
| Answer what the line is holding back | Yes: `view` reports the held parent block | `view` |

## Caller example

BranchTool decides what the calling line may do; the view is one read on the line's own BranchLine, and no `Branch` object is involved.

```rust
// The tool execution carries the calling line id as a value.
let line = line_owner::<BranchLine>(&execution.branch_id)?.view().await?;
match line.role {
    LineRole::Tool => { /* may end itself, message its parent, read its children */ }
    LineRole::Host => { /* follows its creator's protocol; message refused */ }
    LineRole::Main | LineRole::User => { /* opens and reads lines, ends none of them */ }
}
if line.ending {
    return refused("branch_ended", &execution.branch_id);
}
let after = line.cursor.unwrap_or(cut);
```

## Own contract

### Contract dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | The line's own facts, in commit order after the rebuild. |
| `B:Timeline` / `B:TimelineIdentity` | Its own facts and its own id. |
| `B:BranchLifecycle` | Wait for the branch to be active before writing. |
| `B:InputQueue` | Its own line's input entry. The opener's entry is called by line id (`line_owner::<InputQueue>`), not injected (ADR 027 D4). |
| `S:SessionScope` | `activated` and `closing`: wait for the Session's publication before roster repair. |
| `S:GuideEntries` | The four markers above, until this part unloads. |
| owner-internal `TreeHandle` | The owner's weak handle to TimelineTree: the line's record, the default line, the move transitions and the parent's turn-end signal. Weak because the tree holds this line's scope and the scope holds this part; a strong handle would be a cycle close could not break. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors (codes) |
| --- | --- | --- | --- | --- | --- |
| `view` | One line's role, parent, end state and the read cursor for the target the caller asks about | `{target?}` (the operation runs on the line's own BranchLine) | `{role, parent?, ending, cursor?, blocked?}` | None: a read of the owner's folded state, or one bounded read of its own history for that target | `unknown` (the line has no directory record yet), `storage` (the read of its own history failed; retryable), `closed` |

`cursor` is the `nextCursor` of this line's latest successful read of the named target: retained from live results, and rebuilt on load from this line's own `step/tool-use` and `step/tool-result` for that target when the line has read it. It is absent when the target is omitted or this line never read it. `blocked` reports the held parent block: its target and the head it reported. The reply is a value, not a subscription: BranchTool reads it once per call, and the owner keeps it current from its own facts.

### Errors

| Kind (wire code) | What the caller does next |
| --- | --- |
| `unknown` | The line has no directory record yet; retry after it is published, or treat the line as unopened |
| `storage` | Retryable: the line's own history could not be read, so no cursor is known; retry the view, or read with no `after` |
| `closed` | The Session is closing; the caller's own refusal follows |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `view` | Read the cached role and the target's cursor; a target with no cursor reads that target's facts once | Nothing to release | Nothing to release | Refused `closed` for a closing Session; a reloaded line rebuilds from its own facts | BranchLine |
| internal writes | A fact of the line's own arrives, the fold decides, one keyed block is written into its own line through its input entry. The ended notice is one **Steer** input carrying the roster's keyed block when the roster retires, so an idle opener wakes once; only the idle notice steers the opener through `line_owner::<InputQueue>(opener)` | The write belongs to the line, not to a caller; a cancelled turn writes nothing more | The line's writes stop with the line | The branch scope closes after its last write; a restored line rebuilds its blocks and writes only what is owed | BranchLine writes; the line's input owner admits; TimelineTree settles the scope |

## Exceptions with reasons

- The status note restates the line's birth identity byte for byte: a compaction replays the note, so it must be self-contained; the identity is written by TimelineTree and repeated here only as text. TimelineTree records the move; this owner writes the note.
- A restored line writes a missing note once and, on the conversation line, replaces a note an older rule set wrote: the note's text is this owner's, and the tree only knows the transition.
- The parent block is decided from the line's own state rather than from the parent's signal: a signal that arrives while a block is held must not lose the move, so the queue and the block's consumption decide the write.
- The idle notice is steered into the opener's line through the opener's input owner (`line_owner::<InputQueue>`), not by TimelineTree: the notice is this line's statement about itself, and the opener's queue decides its order. The ended notice and the roster retirement are written by the opener's own BranchLine on its own line when it folds `branch/ended`, because the ended line's scope is closing and its writer must not be the one to tell the opener.

## Verification

Flow tests on the owner:

- A line cut from the root knows `main`, `tool` or `host` from its opening facts and reports no cursor before its first read.
- The roster lists the open tool lines in opening order, retires when none is open, and a child cut after its parent wrote a roster gets one retirement block.
- An idle tool line steers its opener once per turn end that said nothing, and never while it is the conversation line.
- A line ended from outside wakes its opener once: the opener's BranchLine writes the roster retirement and `<dim-branch-ended>` on its own line, and the ended line's closing scope writes nothing to it.
- A move writes one status note per participant in commit order; a reload of the conversation line replaces an outdated note once and writes nothing on a second restart.
- A parent turn end past the cursor writes one parent block; a block held while a move arrives is written after consumption, deletion or interruption.

## Supersedes

- [Timeline branches](../../../design/timeline-branches.md): §5's BranchLine row and its "BranchLine injects …" sentence, and §7.1.1's parent-block write paragraph, each now points here.

## Client-visible changes

The `branchLine` face keeps its wire name and its scope but replaces its three operations (`resolved`, `ending`, `cursor`) with one `view` that takes `{target?}` and answers `{role, parent?, ending, cursor?, blocked?}`. The generated `npm/branch-services.d.ts` loses the three and gains `view`, and the runtime fixture `runtime/fixtures/branch-part.ts`, which calls them today, moves to `view` in the same change. No RPC client calls this face; a foreign part that bound one of the three must move to `view`, and its caller gets the same facts in one reply.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BranchLine is one branch part and a Capability: it folds its own committed facts, writes its own keyed blocks, and TimelineTree records the move it writes the note for. |
| §2 scopes | Branch scope: role, roster, cursors and notes belong to one line and end with it. |
| §3 orthogonality | It reads its own facts and the directory record; it writes only its own line and steers its opener through the opener's input owner. |
| §4 rule 1 | Its writes are inputs of its own line, admitted once. |
| §4 rule 2 | The ended notice travels as `branch/ended`; the opener's own BranchLine writes its line. The idle notice is steered into the opener's queue. |
| §4 rule 3 | Handlers register tasks; no handler awaits I/O of another owner. |
| §4 rule 4 | Its state is rebuildable from its own facts; the read cursors are re-derived from its own tool results. |
| §4 rule 5 | No batch crosses. |
| §4 rule 6 | The blocks reach the model as inputs of their own line; no other path is added. |
| §4 rule 7 | It uses the tree's handle and its own line's input entry; it reads no node, queue or scope. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Notes are published after their batch commits. |
| §7 | Every argument and reply is data and a line is an id; no object or handle crosses, except the owner's internal weak handle. Deleting the old surface and migrating every caller is one change. The owner answers its own state and facts, never another owner's record. |
| ADR 002 D1 | Branch scope: the line's own view dies with the line. |
| ADR 002 D2 | No wiring plugin: the service is on `BranchLine`; the weak tree handle is an internal reference. |
| ADR 002 D3 | Depends only on the branch services, its own input owner and the tree's handle. |
| ADR 002 D4 | The idle-notice policy is the line's opening data, not a setting of this part. |
| ADR 002 D5 | The contract is ids and values; the Rust binding is the appendix. |
| ADR 027 | BranchTool reaches this owner with `line_owner::<BranchLine>(line)`, a data face of the branch group (D1–D3); a Session part sees `SessionClosing` and `NoLine`, never `NoSession`. |

## Requirements raised to mechanism owners

None beyond TimelineTree's: BranchTool is a Session part and reaches this owner with `line_owner::<BranchLine>(line)`, which is the runtime's id routing stated there; a Session part sees `SessionClosing` and `NoLine`, never `NoSession` ([#2127](https://github.com/arcships/dim/issues/2127); [SessionScope](https://github.com/arcships/dim/pull/2093) states the Session side).

## Binding appendix

The service is marked on `BranchLine`; the wire name is `branchLine`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineRole {
    Main,
    Tool,
    Host,
    User,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldBlock {
    pub target: String,
    pub head: u64,
}

/// One line's published view. Named apart from TimelineTree's `LineState`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineView {
    pub role: LineRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub ending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<HeldBlock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LineViewError {
    #[error("line {line} has no directory record yet")]
    Unknown { line: String },
    #[error("the line's history could not be read")]
    Storage,
    #[error("the Session is closing")]
    Closed,
}

#[rutis::service(name = "branchLine")]
impl BranchLine {
    pub async fn view(&self, target: Option<String>) -> Result<LineView, LineViewError> {
        // ...
    }
}
```

## Current implementation

The code here still uses the pre-contract names (`TreeCommands`, `treeCommands`); the contract above is the target.

Owner: [timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs).
Folds the line's own committed facts into its role, the tool lines it opened,
the state of its current turn and the line's read cursors, and writes its roster,
the parent block, the idle notice, the ended notice and the one status note a
move of the default branch writes on a participant, through the tree's handle.

Its role and its parent come from
`TreeCommands.info`, which is filled from the parent's `branch/create` at
creation and at restore.

### Data face

The branch-scoped `branchLine` face exposes the owner's existing read operations:
`resolved()` returns `[role, parent]` or `null`, where role is `main` (the root), `tool`, `host` (a plugin line) or
`user` (a line a client opened); `ending()` reports whether the line committed its own `branch/end`;
`cursor(target)` returns its latest successful read cursor for that target or
`null`. Cursor read failures use the standard `AgentError` name, variant code
and display message on the wire; native error source identity is not serialized.
The native binding and foreign dispatcher call these same operations. Reads retain the current cached
role/cursor behavior and load only the requested target's cursor when needed;
they do not read or rebuild all history. Writer lifecycle and event settlement
are unchanged. See [service faces](../../../design/service-faces.md).

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Consume the line's own facts in order after the rebuild. |
| `B:Timeline` | Fix the committed mount cutoff. |
| `B:TimelineIdentity` | The line's own id. |
| `B:BranchLifecycle` | Wait for the branch to be Active before writing. |
| `S:SessionLifecycle` | Wait for the restored child directory to be published before roster repair. |
| `S:TreeCommands` | `info` for the role and its lines; `head`, the crate-internal `turn_ends` and the parent's new-activity check for the parent block; `default_branch`, the crate-internal move signal and the line's own queue of owed moves for the status note; `input` for every write. |
| `S:GuideEntries` | Register the roster and message entries on every line, until this part unloads. |

### Events and lifetime

Consumes `branch/create`, `branch/create-failed`, `branch/ended`, `branch/end`,
`input/message`, `input/applied`, `input/consumed`, `input/deleted`, `turn/start`,
`turn/end`, `stop/interrupt`, `step/tool-use` and `step/tool-result`. It writes only
`input/message`, through the tree's input path.

- **Status note.** On a line that is not the root, one keyed Append under `branch:line` when the
  line becomes the default branch or stops being it, read in commit order from the queue the tree
  filled when the move committed, woken by its move signal. It
  restates the line's birth identity byte for byte and then the sentences the move changes; the
  root never receives one, and a load whose latest block disagrees with the tree writes the
  missing note once; on the tree's current default branch the load also compares that block's text
  with the note this code writes, and replaces a note an older rule set left once (design [§5](../../../design/timeline-branches.md#5-parts), [§12.1](../../../design/timeline-branches.md#121-context-mapping)).
- **Roster.** On a line that has opened tool lines, a keyed Append under `branch:roster` of `<dim-branch-lines>` lists the lines it opened with the tool that are still open, in the order it opened them, with the
  first line of each purpose; when none is open the key is retired. A plugin line is never on a roster, and neither is a line the user opened: it stays in the Session's directory and coordinates its own tool lines. A child whose inherited prefix holds a live roster gets one block in its opening input that retires that key, so a line that opened no tool line of its own keeps no list; TimelineTree writes it with the identity block (design [§7.1](../../../design/timeline-branches.md#71-keyed-state-branchline-branchroster-and-branchparent)).
- **Parent block.** On a line that is not the root, one keyed Append of
  `<dim-branch-parent>` on the line itself when its opener ends a turn past
  the line's read cursor for it, the head the latest block reported, and the
  line's opening, the sequence of its `branch/create` on the parent. It names
  the parent, that head and the cursor, and never carries the parent's
  content. The fold keeps the cursor of each successful read from the line's
  own `step/tool-use` and `step/tool-result`, so a replay rebuilds it. The block is
  decided from the line's state rather than the parent's signal: the parent's
  turn-end, published by TimelineTree, and the line's own write queue wake the
  writer, and consuming, deleting or interrupting a held block queues the same
  decision. An idle line is not woken, and the root gets none. A child whose
  inherited prefix holds a live parent block, meaning a nested line, gets one
  more block in its opening input, naming its own parent at the cut;
  TimelineTree writes it with the identity block
  (design [§7.1.1](../../../design/timeline-branches.md#711-the-parent-block-branchparent)).
- **Idle notice.** On a line opened by the tool, one Steer to its opener at a `turn/end` that
  neither steered the opener in that turn, called `end`, nor left a driving
  input pending, and only while the line is not the current default branch. A steer counts only once its tool result says the call was
  accepted: a refused or denied call wrote nothing to the parent, which would
  otherwise wait on silence.
- **Ended notice.** On the opener, the roster update and `<dim-branch-ended>` in one
  Steer, for any end of a tool line it opened other than that line's own `end`.
- **Status note.** On the new default branch and on the former one, one keyed Append of
  `<dim-branch-line>` under `branch:line`, carrying `default`, restating the identity it replaces
  and then the rules that hold while the line is, or is no longer, the default branch. The key is
  the identity's, so a compaction replays the note and the line still knows which line it is. The
  note states no rule another line could contradict: the protection of the default branch's chain
  is decided when `end` is called, from the tree. The root gets no note, because neither of its
  rules changes with the role, so a note to it would say nothing.

BranchLine mounts at the committed Dispatcher cutoff without reading historical events. It starts its writer after both branch and Session publication so all current child directory entries are available. Roster repair reads known current creation positions and the last roster block. A root whose stored history was written before the key split also holds its old roster under `branch:line`; its load reads that block once and retires the key with the roster write, alone when none is owed, so the root keeps no identity, as a root has none. Parent notices read the last parent block and the requested parent's read cursor. Old turn progress is not restored; a recovery turn end does not emit an idle notice. Read cursors are loaded for one target when used, and neither ordinary input history nor all tool results are retained. No recovery event is written.

### Tests

[branch_flow.rs](../../../../crates/dim-agent-capabilities/tests/branch_flow.rs):
`the_root_lists_its_open_tool_lines_under_the_roster_key`,
`a_configured_roster_closing_reaches_the_roster`,
`an_idle_line_tells_main_once_when_its_turn_ends_without_a_word`,
`a_tool_line_ends_itself_and_main_learns_it_from_the_roster`,
`an_end_from_outside_a_tool_line_wakes_main_with_a_notice`,
`a_roster_and_an_identity_replay_separately_after_a_compaction`,
`a_child_cut_after_a_roster_retires_it_and_one_cut_before_gets_nothing`,
`the_roots_roster_from_before_the_key_split_is_retired_at_load`,
`an_opener_that_is_not_the_root_keeps_its_identity_and_its_roster`,
`a_status_note_and_a_live_roster_do_not_retire_each_other`,
`a_user_opened_default_branch_coordinates_the_line_it_opens`,
`a_line_messages_and_reads_its_parent_and_the_lines_it_opened`,
`a_sibling_or_grandparent_is_refused_with_nothing_written`,
`an_idle_line_is_told_its_parent_moved_without_being_woken`,
`a_running_line_gets_the_block_with_its_next_request_once`,
`two_parent_turns_while_the_line_is_idle_write_one_block`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_consumed`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_deleted`,
`a_move_while_a_block_is_held_is_written_after_the_block_is_interrupted`,
`a_restore_writes_a_due_parent_block_once_and_none_without_movement`,
`an_ending_line_and_main_get_no_parent_block`,
`the_parent_block_replays_after_a_compaction`,
`the_creating_call_is_written_before_the_line_was_opened`,
`a_host_line_cut_at_the_start_is_not_told_what_happened_before_it`,
`a_host_line_cut_at_the_start_is_told_when_main_acts_after_it`,
`a_host_line_cut_before_it_was_opened_is_not_told_about_the_gap`,
`a_nested_line_is_told_its_own_parent_at_open`,
`a_childs_first_system_is_its_parents_at_the_cut`.

<!-- generated:plugin BranchLine — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:GuideEntries`、`S:SessionLifecycle`、`S:TreeCommands` |
| 提供 | `B:BranchLine`、`B:dyn HostDispatch#branchLine` |
| 被注入于 | — |
<!-- /generated -->
