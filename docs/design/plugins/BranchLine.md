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
- **The status note.** One keyed block when the line becomes, or stops being, the conversation line, rebuilt from the move the line took part in.

Callers keep: which lines they message or read (BranchTool's kin rules), what a message says, how a page is rendered, and every decision about the caller's own work.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| BranchTool asks this line's role before deciding what it may message, read or end | No: the role and parent are folded from the line's own facts and cached; re-deriving them in a Session part means folding another owner's state | `state` |
| BranchTool learns whether this line has committed its own end | No: same fold | `state` |
| BranchTool continues a read of a target where the line's last read stopped | Yes, once the cursor is published with the state: one more field of the same read | `state` |
| Write the roster, the parent block, the idle notice, the ended notice, the status note | No, but not public: these are this owner's own timeline writes, driven by the line's facts | Internal |
| Tell the opener the line moved: a parent turn end past the cursor | No: it is this owner's fold of its own facts and its own write queue | Internal |
| Answer what the line is holding back | Yes: `blocked` reports the held parent block | `state` |

## Caller example

BranchTool decides what the calling line may do; the state is one read at the line id, and no `Branch` object is involved.

```rust
// The tool execution carries the calling line id as a value.
let line = branch_line::state(execution.branch_id.clone()).await?;
match line.role {
    LineRole::Tool => { /* may end itself, message its parent, read its children */ }
    LineRole::Host => { /* follows its creator's protocol; message refused */ }
    LineRole::Main | LineRole::User => { /* opens and reads lines, ends none of them */ }
}
if line.ending {
    return refused("branch_ended", &line_id);
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
| `S:SessionLifecycle` | Wait for a restored directory before roster repair. |
| `S:TimelineTree` | The line's record by id, the default line, the move signal and the parent's turn-end signal. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `state` | One line's role, parent, end state and read cursors | `{line}` | `{role, parent?, ending, cursors: [{target, cursor}], blocked}` | None: a read of the owner's folded state | `unknown` (no directory record), `closed` |

`blocked` reports the held parent block: its target and the head it reported. `cursors` holds one entry per target the line has read; an absent target is a line this line never read. The reply is a value, not a subscription: BranchTool reads it once per call, and the owner keeps it current from its own facts.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `unknown` | The line has no directory record yet; retry after it is published, or treat the line as unopened |
| `closed` | The Session is closing; the caller's own refusal follows |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `state` | Read the cached role and cursors; a missing cursor reads that target's facts once | Nothing to release | Nothing to release | Refused `closed` for a closing Session; a reloaded line rebuilds from its own facts | BranchLine |
| internal writes | A fact of the line's own arrives, the fold decides, one keyed block is written through the line's input entry | The write belongs to the line, not to a caller; a cancelled turn writes nothing more | The line's writes stop with the line | The branch scope closes after its last write; a restored line rebuilds its blocks and writes only what is owed | BranchLine; TimelineTree settles the scope |

## Exceptions with reasons

- The status note restates the line's birth identity byte for byte: a compaction replays the note, so it must be self-contained; the identity is written by TimelineTree and repeated here only as text.
- A restored line writes a missing note once and, on the conversation line, replaces a note an older rule set wrote: the note's text is this owner's, and the tree only knows the transition.
- The parent block is decided from the line's own state rather than from the parent's signal: a signal that arrives while a block is held must not lose the move, so the queue and the block's consumption decide the write.

## Verification

Flow tests on the owner:

- A line cut from the root knows `main`, `tool` or `host` from its opening facts and reports no cursor before its first read.
- The roster lists the open tool lines in opening order, retires when none is open, and a child cut after its parent wrote a roster gets one retirement block.
- An idle tool line steers its opener once per turn end that said nothing, and never while it is the conversation line.
- A move writes one status note per participant in commit order; a reload of the conversation line replaces an outdated note once and writes nothing on a second restart.
- A parent turn end past the cursor writes one parent block; a block held while a move arrives is written after consumption, deletion or interruption.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BranchLine is one branch part and a Capability: it folds its own committed facts and writes its own keyed blocks. |
| §2 scopes | Branch scope: role, roster, cursors and notes belong to one line and end with it. |
| §3 orthogonality | It reads its own facts and the directory record; it writes only its own line. |
| §4 rule 1 | Its writes are inputs of its own line, admitted once. |
| §4 rule 2 | Parent and ended notices travel as facts to the opener; the opener's own BranchLine writes its line. |
| §4 rule 3 | Handlers register tasks; no handler awaits I/O of another owner. |
| §4 rule 4 | Its state is rebuildable from its own facts; the read cursors are re-derived from its own tool results. |
| §4 rule 5 | No batch crosses. |
| §4 rule 6 | The blocks reach the model as inputs of their own line; no other path is added. |
| §4 rule 7 | It uses the tree's operations; it reads no node, queue or scope. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Notes are published after their batch commits. |
| §7 callbacks and ids | `state` takes a line id and returns a value; no callback and no handle cross. |
| §7 one change | The implementation deletes `resolved`, `ending` and `cursor` and migrates BranchTool in the same change. |
| ADR 002 D1 | Branch scope: the line's own view dies with the line. |
| ADR 002 D2 | No wiring plugin: the service is on `BranchLine`. |
| ADR 002 D3 | Depends only on the branch services and the tree. |
| ADR 002 D4 | The idle-notice policy is the line's opening data, not a setting of this part. |
| ADR 002 D5 | The contract is ids and values; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None beyond TimelineTree's: BranchTool is a Session part and reaches this owner by line id, which is the addressing requirement stated there (#2025 owns the surface).

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

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineState {
    pub role: LineRole,
    pub parent: Option<String>,
    pub ending: bool,
    pub cursors: Vec<Cursor>,
    pub blocked: Option<HeldBlock>,
}

#[rutis::service(name = "branchLine")]
impl BranchLine {
    pub async fn state(&self, line: String) -> Result<LineState, LineViewError> {
        // ...
    }
}
```
