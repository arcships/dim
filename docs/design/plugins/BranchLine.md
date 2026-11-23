# BranchLine

Scope: branch. Issue: #2024.

BranchLine is one line's view of itself and of the lines it opened. It folds the line's own committed facts into its role, the tool lines it opened, the state of its current turn and its read cursors, and it writes the blocks a line's neighbourhood reads: the roster, the parent block, the idle notice, the ended notice and the status note a default-line move leaves. The current implementation is described in the [BranchLine section of the plugin page](../../architecture/rust-agent/plugins/BranchTool.md#branchline).

TimelineTree owns the directory and the lifecycle; BranchLine owns what one line says about its neighbourhood and keeps it current. A line that opened nothing and is the root writes no block.

## Use and scope: responsibility split

BranchLine owns:

- **Its role and parent.** `main`, `tool`, `host` or `user`, and the line it was cut from, both read from the parent's `branch/create` at the line's opening cut — never parsed from rendered history.
- **The tool lines it opened and their states.** The roster of open tool lines with the first line of each purpose, in opening order, retired when none is open.
- **Its read cursors.** The `nextCursor` of its latest successful read of each target it may read.
- **The parent block.** The head and cursor the line has reported to itself about the line it was cut from, and whether a block is currently held and undelivered.
- **Its turn and notices.** The state of its current turn, the idle notice to its opener, the ended notice on its opener.
- **The status note.** One keyed block when the line becomes, or stops being, the conversation line, with the text this owner writes after TimelineTree records the transition.
- **The model-facing markers.** `<dim-branch-lines>`, `<dim-branch-message>`, `<dim-branch-idle>` and `<dim-branch-ended>`, and the guide entries for them, until this part unloads.

Callers keep: which lines they message or read (BranchTool's kin rules), what a message says, how a page is rendered, and every decision about the caller's own work.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| BranchTool asks this line's role before deciding what it may message, read or end | No: the role and parent are folded from the line's own facts and cached; re-deriving them in a Session part means folding another owner's state | `view` |
| BranchTool learns whether this line has committed its own end | No: same fold | `view` |
| BranchTool continues a read of a target where the line's last read stopped | Yes, once the cursor is published with the view: one more field of the same read | `view` |
| Write the roster, the parent block, the idle notice, the ended notice, the status note | No, but not public: these are this owner's own writes, driven by the line's facts | Internal |
| Tell the opener the line moved: a parent turn end past the cursor | No: it is this owner's fold of its own facts and its own write queue | Internal |
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
let after = line.cursor(&target).unwrap_or(cut);
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | The line's own facts, in commit order after the rebuild. |
| `B:Timeline` / `B:TimelineIdentity` | Its own facts and its own id. |
| `B:BranchLifecycle` | Wait for the branch to be active before writing. |
| `B:InputQueue` | Its own line's input entry: every block this part writes is an input of its own line. |
| `S:SessionLifecycle` | Wait for a restored directory before roster repair. |
| `S:GuideEntries` | The four markers above, until this part unloads. |
| `S:TreeHandle` | The owner's weak handle to TimelineTree: the line's record, the default line, the move transitions and the parent's turn-end signal. Weak because the tree holds this line's scope and the scope holds this part; a strong handle would be a cycle close could not break. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors (codes) |
| --- | --- | --- | --- | --- | --- |
| `view` | One line's role, parent, end state, read cursors and held parent block | `{}` (the operation runs on the line's own BranchLine) | `{role, parent?, ending, cursors: [{target, cursor}], blocked?}` | None: a read of the owner's folded state | `unknown` (the line has no directory record yet), `closed` |

`blocked` reports the held parent block: its target and the head it reported. `cursors` holds one entry per target the line has read; an absent target is a line this line never read. The reply is a value, not a subscription: BranchTool reads it once per call, and the owner keeps it current from its own facts.

### Errors

| Kind (wire code) | What the caller does next |
| --- | --- |
| `unknown` | The line has no directory record yet; retry after it is published, or treat the line as unopened |
| `closed` | The Session is closing; the caller's own refusal follows |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `view` | Read the cached role and cursors; a missing cursor reads that target's facts once | Nothing to release | Nothing to release | Refused `closed` for a closing Session; a reloaded line rebuilds from its own facts | BranchLine |
| internal writes | A fact of the line's own arrives, the fold decides, one keyed block is written into its own line through the line's input entry; the idle and ended notices steer the opener through `line_owner::<InputQueue>(opener)` | The write belongs to the line, not to a caller; a cancelled turn writes nothing more | The line's writes stop with the line | The branch scope closes after its last write; a restored line rebuilds its blocks and writes only what is owed | BranchLine writes; the line's input owner admits; TimelineTree settles the scope |

## Exceptions with reasons

- The status note restates the line's birth identity byte for byte: a compaction replays the note, so it must be self-contained; the identity is written by TimelineTree and repeated here only as text. TimelineTree records the move; this owner writes the note.
- A restored line writes a missing note once and, on the conversation line, replaces a note an older rule set wrote: the note's text is this owner's, and the tree only knows the transition.
- The parent block is decided from the line's own state rather than from the parent's signal: a signal that arrives while a block is held must not lose the move, so the queue and the block's consumption decide the write.
- The idle and ended notices are written into the opener's line through the opener's input owner (`line_owner::<InputQueue>`), not by TimelineTree: the notice is this line's statement about itself, and the opener's queue decides its order.

## Verification

Flow tests on the owner:

- A line cut from the root knows `main`, `tool` or `host` from its opening facts and reports no cursor before its first read.
- The roster lists the open tool lines in opening order, retires when none is open, and a child cut after its parent wrote a roster gets one retirement block.
- An idle tool line steers its opener once per turn end that said nothing, and never while it is the conversation line.
- A move writes one status note per participant in commit order; a reload of the conversation line replaces an outdated note once and writes nothing on a second restart.
- A parent turn end past the cursor writes one parent block; a block held while a move arrives is written after consumption, deletion or interruption.

## Supersedes

- The `branchLine` data face (`resolved`, `ending`, `cursor`) and its section of the [TimelineTree plugin page](../../architecture/rust-agent/plugins/TimelineTree.md): one `view` replaces the three reads.
- The `TimelineTreeControl`-era dependency description of BranchLine in [timeline branches §5](../../design/timeline-branches.md#5-parts) as far as it names `S:TreeCommands`: the handle is the owner's weak `S:TreeHandle`.

## Client-visible changes

None: the `branchLine` wire name keeps its scope and gains one operation; no client calls it today.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BranchLine is one branch part and a Capability: it folds its own committed facts, writes its own keyed blocks, and TimelineTree records the move it writes the note for. |
| §2 scopes | Branch scope: role, roster, cursors and notes belong to one line and end with it. |
| §3 orthogonality | It reads its own facts and the directory record; it writes only its own line and steers its opener through the opener's input owner. |
| §4 rule 1 | Its writes are inputs of its own line, admitted once. |
| §4 rule 2 | Parent and ended notices travel as facts; the opener's own BranchLine writes its line. |
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
| ADR 027 | BranchTool reaches this owner with `line_owner::<BranchLine>(line)`, a data face of the branch group. |

## Requirements raised to mechanism owners

None beyond TimelineTree's: BranchTool is a Session part and reaches this owner with `line_owner::<BranchLine>(line)`, which is the typed resolution requirement stated there ([SessionScope](https://github.com/arcships/dim/pull/2093) owns the mechanism side).

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
pub struct Cursor {
    pub target: String,
    pub cursor: u64,
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
    pub cursors: Vec<Cursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<HeldBlock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LineViewError {
    #[error("line {0} has no directory record yet")]
    Unknown(String),
    #[error("the Session is closing")]
    Closed,
}

#[rutis::service(name = "branchLine")]
impl BranchLine {
    pub async fn view(&self) -> Result<LineView, LineViewError> {
        // ...
    }
}
```
