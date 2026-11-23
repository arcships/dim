# TimelineTree

Scope: Session. Issue: #2024.

TimelineTree owns a Session's lines: the directory of what lines exist, each line's beginning and end, and the facts a line's neighbours learn. A line is one branch scope with an id; the RPC field naming a line stays `branchId`. The current implementation is described in the [TimelineTree plugin page](../../architecture/rust-agent/plugins/TimelineTree.md).

TreeTimeline, BranchLine and BranchTool are separate owners in this group with their own documents. A line's own owners — its input queue, its capabilities, its Prompt — are other owners; this contract says how a Session part reaches a line's owners and provides no entry into them.

## Use and scope: responsibility split

TimelineTree owns:

- **The line directory.** Every line's id, parent, cut, source, state (open, ending, ended), head and end event, and the line the Session's conversation is on. One read answers the default together with the whole directory, so a client never pairs a default from before a move with a directory from after one.
- **A line's beginning.** Who may open a line, how many may be open, the cut's validity, building the branch scope, publishing it, and rolling it back when it fails. The opening blocks a child starts with — identity, the retired roster, the parent block — are written here because the child has no owner yet.
- **A line's end.** The target checks, ending descendants before their parent, closing the scope, `branch/end` on the line, `branch/ended` on the parent, and the final message carried as data on the end fact. An end that is admitted always settles.
- **Ended lines' history.** A line's scope is gone once ended; its facts stay readable at the line's fixed head for as long as the Session lives, from the same store the live line used.
- **Each live line's scope.** Created, held and closed here. A line's scope never outlives its end, and no handle to it leaves this owner.

Callers keep:

- What a line is for: a goal, a collaboration member, a tool line, a plugin line, a terminal line, a client's line.
- The texts the model reads and the guide entries for the markers, except the opening blocks this owner writes.
- Which facts they fold, and every decision about their own line.
- Which line a request names when a client or a method may name none: they read `default` first.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client opens a conversation line at a position it names | No: eligibility, the Session's cap and the cut must be charged in the same lock that reserves the line, or two opens slip past the cap | `create` |
| The model opens a tool line cut before its own request | No: same admission, and the tool line's opening blocks are written by this owner | `create` |
| A plugin opens a line it will drive | No: same admission; the line's source decides that a plugin line may never be the default | `create` |
| End a line: a tool line ending itself with a last word, a user ending a line, a plugin ending what it opened | No: the target's guards, ending descendants first, closing the scope and recording the end are one settled operation | `end` |
| Move the conversation to another line and tell every participant | No: the pointer, the `branch/default` fact and the per-line notes commit in one order | `set_default` |
| Read the lines and the current default | No: `default` and the directory come from one read; states, heads and end events are this owner's | `directory` |
| Read one line's record: is it open, where is its head, when did it end | No: the record is the directory's own entry | `line` |
| Read a line's facts at a cutoff: a client's history, a method's turn, a tool's recall, an index build, a report's evidence | No: the cutoff's validity, the atomic-batch boundary, the fixed head of an ended line and the continuation rule are this owner's knowledge | `read` |
| Read a line and its ancestors as one cut: a search over the chain, an index over the chain | No: each ancestor is bounded by the cut the line inherited from it, and the chain is the directory's knowledge | `read` with the chain scope |
| Read a line's purpose and source for its detail view | Yes: `line` carries the cut and source; the purpose is the parent's `branch/create` facts at that cut | Business composes with `line` and `read` |
| Learn whether a line is still open before writing to it | Yes: the record's state answers open, ending or ended | Business composes with `line` |
| Learn the configuration a line adopted at a cutoff | Yes for a live line: Prompt answers it through `line_owner`; for an ended line the facts are read at the fixed head | Business composes with `read`; Prompt keeps its own operation |
| Write an input to a line | Not this owner: the line's input owner decides order, idempotency and rejection | `line_owner::<InputQueue>`, see "Session parts reaching a line's owners" |
| Act on a line's own owner: compact it, read its usage, withdraw from it, approve in it, adopt a language or a profile in it | Not this owner: each is that owner's operation, reached by line id | `line_owner::<T>`, see "Session parts reaching a line's owners" |
| Create the root line, restore the stored directory, settle on close | No, but not public: the Session assembly drives these; a client or plugin must not | Assembly-internal |

## Caller example

A client opens a line, reads the directory and ends a line; a Session method answers a turn read with one page. No call has a version to compare, a wait to bound, or an id the caller must release.

```rust
// session.branch_create: the client names the parent, the cut and the purpose.
let line = tree.create(CreateLine {
    parent: parent.clone(),
    cut,
    prompt,
    source: BranchSource::User { operation_id },
    idle_notice: None,
}, cancelled).await?;

// session.branches: the default and every line from one read.
let Directory { default, lines } = tree.directory()?;
let current = lines.iter().find(|record| record.line == default).expect("the default is a line");

// A Session method answering a turn read on the conversation line, at its head.
let page = tree.read(ReadLines {
    line: current.line.clone(),
    scope: None,
    from: Some(after + 1),
    through: Some(current.head),
    fields: TURN_FIELDS.to_vec(),
    limit: None,
}).await?;

// A method that names a line reads its record first, then the same page.
let record = tree.line(requested.clone())?;
let page = tree.read(ReadLines {
    line: requested,
    scope: None,
    from: None,
    through: Some(record.head),
    fields: TURN_FIELDS.to_vec(),
    limit: None,
}).await?;

// session.branch_end: the client ends the line it opened.
tree.end(EndLine { line, source, final_message: None }).await?;

// Nothing is released here: the line id is a value and the record's head is a number.
```

A caller that needs the line's own owners — the input queue it writes into, the capability it compacts — calls no operation of this owner for that; the next section says how a Session part reaches those owners. The Session assembly drives the root line, the restore and the close through the owner's crate-private lifecycle, not through these operations, and no client or plugin may reach them.

## Session parts reaching a line's owners

A Session part acts on one line by line id, never through a `Branch` object. Two families of operation exist, and neither is TimelineTree's:

- **Change the line's state: write an input.** The target line's input owner (`inputQueue.submit` in that line's scope) accepts it in order and decides the outcome. TimelineTree is not in this path.
- **Read the line's published state: read the value the owner publishes.** Each owner publishes its own value; a part reads it at the line id. TimelineTree answers which line is the conversation line and what state a line is in; it never answers for another owner.

The provider is the mechanism, and its shape is typed: `line_owner::<T>(line)` returns the branch-scope service `T` of the live instance the runtime built for that line, and the caller then calls that owner's own operation. `line_owner` answers the caller's context with the service; a line that is not live (ended, or never this Session's) answers `NoInstance`, and a closing Session answers `Closed`. No operation name is a string, no request is a `json!` value, and no forwarding facade sits between the caller and the owner. The mechanism keeps the `(session, line)` index it builds when it constructs a line's context; TimelineTree is not in this path.

Caller code after the change: a Session method steering a member line, and a method reading a line owner's published value.

```rust
// Write the line's state: the member's input owner decides order and outcome.
line_owner::<InputQueue>(&member)?.submit(steer_input, cancelled).await?;

// Read a line's published value: the owner answers, not its timeline.
let usage = line_owner::<ContextUsage>(&line)?.read(None, cancelled).await?;
```

A Session part that cannot resolve a line keeps its business blocked; it does not fall back to holding a `Branch`, and TimelineTree adds no forwarding operation for it. [SessionScope](SessionScope.md) documents the mechanism side with this contract.

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionScope` | `identity` for the Session id the store is keyed by, and `openBranch` to construct a line's context; the tree holds and settles what `openBranch` returns. |
| `S:dyn TimelineStore` | The line directory and a line's facts in storage, at a fixed head. |
| `S:GuideEntries` | The `<dim-branch-line>` and `<dim-branch-parent>` entries, until this part unloads. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `create` | Open a line at a cut of its parent | `{parent, cut, prompt, source, idle_notice?}` | `{line}` | While the line is admitted and built; an admitted creation is not abandoned and a cancelled one is rolled back before the reply | `invalid_parent` (no such line), `parent_ended`, `invalid_cut` (over the parent's head, or not an atomic-batch boundary), `not_opener` (this line may not open one), `limit_reached` (the Session's cap of open tool lines), `closed`, `cancelled` |
| `end` | End a line and settle it | `{line, source, final?}` | `{}` | None: an admitted end settles even if the caller goes away | `unknown`, `already_ended`, `protected` (the root, the default line, or an ancestor of the default line), `final_undelivered` (the line ended and its final message is still owed; a later end retries delivery only), `closed` |
| `set_default` | Move the conversation to a line | `{line, source}` | `{default}` | None: the move is one commit | `unknown`, `already_default`, `not_selectable` (a plugin line, a line that is ending, or a line whose chain is ending), `closed` |
| `directory` | The lines and the current default, from one read | `{}` | `{default, lines}` | None | `closed` |
| `line` | One line's record | `{line}` | `record` | None | `unknown`, `closed` |
| `read` | A line's facts at a cutoff, or the line and its ancestors at one cut | `{line, scope?, from?, through?, fields?, limit?}` | `{through, events}` or `{through, rows}` | None: one bounded read, and the reply is the cutoff's answer | `unknown`, `beyond_head` (a cutoff past the line's head), `not_batch_boundary` (a cutoff inside an atomic batch), `continuation_without_cutoff` (a continuation page with no original cutoff), `closed` |

A record is `{line, parent, cut, source, state, head, end_event?}`; `state` is `open`, `ending` or `ended`. `read` answers a live line from its scope and an ended line from the store at its fixed head, with the same shape and the same visibility. The chain scope reads only published facts, bounds every ancestor at the cut the line inherited, and keeps the original cutoff across a continuation page. `ids` and sizes cross as strings and numbers; no handle, cursor or channel crosses.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_parent` | Pick an existing line, or read `directory` first |
| `parent_ended` | The parent cannot take a child; pick a live line |
| `invalid_cut` | Use the parent's head from `directory`, or a cutoff that ends an atomic batch |
| `not_opener` | Only the root and the conversation line open lines; ask one of those, or move the conversation first with `set_default` |
| `limit_reached` | End an open tool line, or open on a line whose chain is protected |
| `unknown` | `line` or `read` named an id this Session never had; read `directory` for the lines in force |
| `already_ended` | Nothing happened; read the line with `read` |
| `protected` | The root, the conversation line and its ancestors cannot end; move the conversation first |
| `final_undelivered` | The line is ended; call `end` again to retry only the message, which never delivers twice |
| `already_default` | Nothing happened |
| `not_selectable` | A plugin line and a line whose chain is ending cannot be the conversation line |
| `beyond_head` | Lower the cutoff to the record's head |
| `not_batch_boundary` | Use a reply's `through`, or the head of a directory record |
| `continuation_without_cutoff` | Repeat the original `through` on the continuation page |
| `closed` | This Session is closing or unloading; a follow-up decision waits for the Session's replacement |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Validate parent, cut, opener and cap under one lock, reserve the line, build the scope, write the opening blocks and the first input, publish, reply the id | The admission stops; a built but unpublished line is rolled back and its slot released; the caller sees `cancelled` | The line is built and published for the Session: an admitted creation is not abandoned; a waiter that left does not cancel it | A line still building is rolled back; the reply is `closed` | TimelineTree: the scope, the reservation and the slot |
| `end` | Check the target, end descendants deepest first, commit `branch/end`, close the scope, write `branch/ended` on the parent, deliver the final message as data | None; the call answers when the end settles | The end continues; the caller's absence changes nothing | An end in flight finishes before the generation closes; the generation's close waits for it and for the store's answer | TimelineTree: the scope's close and the end record |
| `set_default` | Validate the target and its chain, write `branch/default` on the root, queue one note per participant, wake the writers | None | Nothing to release | Refused with `closed` once closing starts | TimelineTree; each participant's BranchLine writes its own note |
| `directory` | Read the default and every record under one lock, resolve each head | None | Nothing to release | Pending reads answer at the state they observed | TimelineTree |
| `line` | Read one record under the tree lock | None | Nothing to release | A line that ended before the call answers until its record leaves; an id the Session never had answers `unknown` | TimelineTree |
| `read` | Resolve the line and the cutoff, check the batch boundary, read the facts, answer with the cutoff | None | Nothing to release | An ended line's read answers from the store; a live line's read answers `closed` once its scope is gone | TimelineTree |

## Exceptions with reasons

- A child's opening blocks are written before the child's scope is published: until publication the line has no other owner, so no other owner's timeline is written.
- `read` resolves a live line through the directory and returns its facts unchanged: facts are shared state under [design rules §4](../../architecture/rust-agent/principles.md), not another owner's folded state, and the tree never folds another owner's record.
- An ended line's facts are answered here because the line's own owners are gone with its scope; the tree keeps the fixed head and the directory record, so there is no second owner to ask.
- The default line's move writes one note per participant into each participant's queue. The note's text is BranchLine's; the tree only records the transition in commit order, because a restart must not lose it.

## Verification

Flow tests on the owner:

- `create` charges opener and cap under the same lock: a second create and a `set_default` interleaved with it cannot both pass a check; a failed creation releases its slot.
- A cancelled create rolls back its scope and writes `branch/create-failed`; a create admitted before the cancellation still publishes and returns its id.
- `end` ends descendants before their parent, records one `branch/ended` each, and refuses the root, the conversation line and its ancestors with `protected` and no write.
- A tool end whose final message cannot be delivered answers `final_undelivered`, and a later `end` delivers the message exactly once.
- `directory` answers `default` and the lines from one read under a concurrent move or end; `line` answers a reservation, an ending line and an ended line, and `unknown` for an id the Session never had.
- `read` answers an ended line at its fixed head with the same field selection as the live line, refuses a cutoff inside an atomic batch, and keeps the original cutoff across a continuation page.
- Close cancels reservations, joins builds, closes every live scope and answers after the store's last batch.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | TimelineTree is one Session part and three branch parts (TreeTimeline, BranchLine, BranchTool in their own documents). TimelineTree writes Timeline facts and folds no state from them: not a Capability. |
| §2 scopes | Session scope for the directory and lifecycle; the branch scope holds TreeTimeline and BranchLine. The Session owns the child scope's lifecycle under ADR 002 D1. |
| §3 orthogonality | No other owner's state is read or changed. The final message is data on the end fact; the opener's BranchLine writes it into its own line. |
| §4 rule 1 | No command event from a client: `create`, `end` and `set_default` write facts only after their admission commits. |
| §4 rule 2 | The end and the move hand off through facts (`branch/end`, `branch/ended`, `branch/default`); the notes and the final message are written by their own line's BranchLine. |
| §4 rule 3 | No handler waits for I/O: creation and settlement run in the owner's tasks, and close joins them. |
| §4 rule 4 | The directory and the facts are the Session's own state; no other owner's record is copied into them. |
| §4 rule 5 | Batch boundaries are enforced by `read` and by `create`'s cut. |
| §4 rule 6 | No event reaches the model from here; the opening blocks are inputs the tree writes, and the model reads them as inputs. |
| §4 rule 7 | Callers use these six operations; no caller reads a node, a queue or a scope. |
| §4 rule 8 | One requirement is raised below: resolving a line id to a branch-scope service. |
| §4 rule 9 | Facts are published after their atomic batch commits; the end and its message are one fact. |
| §7 callbacks and ids | `create` takes the caller's cancellation as an argument; every argument and reply is data, a line is an id, and no object or handle crosses. |
| §7 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| §7 no proxy reads | The tree answers its own directory and the facts, never another owner's record; a line's owners are reached through `line_owner`, not through this owner. |
| ADR 002 D1 | Session scope: the directory outlives every line and every branch scope is created and settled here. |
| ADR 002 D2 | No wiring plugin: the service is on `TimelineTree`; no wrapper type and no trait object remain beside it. |
| ADR 002 D3 | Dependencies point from extensions to the core; TimelineTree depends on the store, the factory and the identity only. |
| ADR 002 D4 | No setting selects a behaviour here; the cap and the admission are configuration the Session is assembled with. |
| ADR 002 D5 | The contract is language-neutral: ids, records and facts; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **Resolve a line id to a branch-scope service.** Provide `line_owner::<T>(line)`: resolve the line to the live instance the runtime built for it (`SessionScope.openBranch`) and answer the branch-scope service `T` there, so the caller calls that owner's own typed operation; `NoInstance` when the line is not live, `Closed` when the Session is closing. A Session part uses it only for the two families decision 2 allows: writing input to the line's input owner, and reading the value a line owner publishes. The mechanism keeps the index; TimelineTree stays out of the path and adds no forwarding operation. [#2025](https://github.com/arcships/dim/issues/2025) and #2093 define the mechanism side with this contract, and Goal, Collaboration, Wait and the tool owners (#2033, #2098, #2103) call the same shape.

2. **The Session access surface stops carrying line operations.** `Session::end_branch`, `set_default_branch`, `branch`, `branches`, `directory`, `default_and_directory`, `ended_head`, `sequence`, `batch_end`, `query` forward here today; after this contract the tree is the only face and the Session surface must not restate it. #2025 owns that surface.

## Binding appendix

The service is marked on `TimelineTree`; the wire name is `timelineTree`. `TreeCommands`, `TimelineTreeControl` and the `Branch` facade no longer cross this owner's boundary. `CancelledFn` is the crate's existing cancellation argument and is not a request field.

```rust
use crate::host::CancelledFn;
use dim_agent_types::{BranchSource, IdleNotice, InputMessageData, TimelinePosition};
use rutis::service;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRecord {
    pub line: String,
    pub parent: Option<String>,
    pub cut: Option<u64>,
    pub source: Option<BranchSource>,
    pub state: LineState,
    pub head: u64,
    pub end_event: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineState {
    Open,
    Ending,
    Ended,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLine {
    pub parent: String,
    pub cut: u64,
    pub prompt: String,
    pub source: BranchSource,
    pub idle_notice: Option<IdleNotice>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndLine {
    pub line: String,
    pub source: BranchSource,
    pub final_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub default: String,
    pub lines: Vec<LineRecord>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineScope {
    Line,
    Chain,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadLines {
    pub line: String,
    pub scope: Option<LineScope>,
    pub scope: Option<LineScope>,
    pub from: Option<u64>,
    pub through: Option<u64>,
    pub fields: Vec<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LineError {
    #[error("no such line: {0}")]
    InvalidParent(String),
    #[error("line {0} ended")]
    ParentEnded(String),
    #[error("cut {cut} is not usable on line {line}")]
    InvalidCut { line: String, cut: u64 },
    #[error("line {0} may not open a line")]
    NotOpener(String),
    #[error("the Session holds its limit of open lines")]
    LimitReached,
    #[error("unknown line: {0}")]
    Unknown(String),
    #[error("line {0} ended")]
    AlreadyEnded(String),
    #[error("line {0} is protected")]
    Protected(String),
    #[error("line {0} ended but its final message is not delivered")]
    FinalUndelivered(String),
    #[error("line {0} is the conversation line")]
    AlreadyDefault(String),
    #[error("line {0} cannot be the conversation line")]
    NotSelectable(String),
    #[error("cutoff {cutoff} is past line {line}'s head {head}")]
    BeyondHead { line: String, head: u64, cutoff: u64 },
    #[error("cutoff {0} is inside an atomic batch")]
    NotBatchBoundary(u64),
    #[error("a continuation needs the original cutoff")]
    ContinuationWithoutCutoff,
    #[error("the Session is closing")]
    Closed,
    #[error("the call was cancelled")]
    Cancelled,
}

impl dim_agent_runtime::PublicService for TimelineTree {}

#[service(name = "timelineTree")]
impl TimelineTree {
    pub async fn create(&self, request: CreateLine, cancelled: CancelledFn) -> Result<String, LineError> {
        // ...
    }

    pub async fn end(&self, request: EndLine) -> Result<(), LineError> {
        // ...
    }

    pub async fn set_default(&self, line: String, source: BranchSource) -> Result<String, LineError> {
        // ...
    }

    pub fn directory(&self) -> Result<Directory, LineError> {
        // ...
    }

    pub fn line(&self, line: String) -> Result<LineRecord, LineError> {
        // ...
    }

    pub async fn read(&self, request: ReadLines) -> Result<ReadReply, LineError> {
        // ...
    }
}
```

`Directory` is `{default, lines}`; `LineRecord` is the record above; `ReadReply` is `{through, events}` or `{through, rows}`. Records, requests and replies are camelCase on the wire; `cut`, `through` and `head` are unsigned integers; a line id is a string.
