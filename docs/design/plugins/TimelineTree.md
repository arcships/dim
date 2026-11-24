# TimelineTree

Scope: Session. Issue: #2024.

TimelineTree owns a Session's lines: the directory of what lines exist, each line's beginning and end, and the facts a line's neighbours learn. A line is one branch scope with an id; the RPC field naming a line stays `branchId`. The current implementation is described in the [TimelineTree plugin page](../../architecture/rust-agent/plugins/TimelineTree.md).

TreeTimeline, BranchLine and BranchTool are separate owners in this group with their own documents. A line's own owners — its input queue, its capabilities, its Prompt — are other owners; a Session part reaches them through `line_owner` (see below), and this contract provides no entry into them.

## Use and scope: responsibility split

TimelineTree owns:

- **The line directory.** Every line's id, parent, cut, source, state (open, ending, ended), head and end event, and the line the Session's conversation is on. One read answers the default together with the whole directory, so a client never pairs a default from before a move with a directory from after one.
- **A line's beginning.** The cut's validity, the caller's eligibility, building the branch scope, publishing it, and the opening blocks the child starts with — identity, the retired roster, the parent block — written where the child has no owner yet.
- **The line cap's enforcement.** The cap is BranchTool's configuration ([timeline branches §6.1](../../design/timeline-branches.md#61-resources-and-limits)); this owner charges it, together with the caller's eligibility and the admitted reservations, in the one lock that reserves the line, counting the open tool lines outside the conversation line's chain.
- **A line's end.** The target checks, ending descendants before their parent, committing `branch/end` with a tool end's final message, delivering that message to the parent, closing the scope, and writing `branch/ended` on the parent — in that order, so a stop after any step leaves one state restore can finish.
- **Ended lines' history.** A line's scope is gone once ended; its facts stay readable at the line's fixed head for as long as the Session lives.
- **Each live line's scope and the weak handle to this owner.** A line's scope is created, held and closed here; the owner's branch parts receive a weak handle to this owner, never a strong one, so a line's scope cannot keep the tree alive.

Callers keep:

- What a line is for: a goal, a collaboration member, a tool line, a plugin line, a terminal line, a client's line.
- The texts the model reads and their guide entries, except the opening blocks this owner writes.
- Which facts they fold, and every decision about their own line.
- Which line a request names when a client or a method may name none: they read `default` first.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client opens a conversation line at a position it names | No: eligibility, the cap and the cut must be charged in the same lock that reserves the line, or two opens slip past the cap | `create` |
| The model opens a tool line cut before its own request | No: same admission, and the tool line's opening blocks are written by this owner | `create` |
| A plugin opens a line it will drive | No: same admission; the line's source decides that a plugin line may never be the default | `create` |
| End a line: a tool line ending itself with a last word, a user ending a line, a plugin ending what it opened | No: the target's guards, ending descendants first, the final message, closing the scope and the end facts are one settled operation | `end` |
| Move the conversation to another line and tell every participant | No: the pointer and the `branch/default` fact commit here, and the transitions are queued for each participant's own writer | `set_default` |
| Read the lines and the current default | No: `default` and the directory come from one read; states, heads and end events are this owner's | `directory` |
| Read one line's record: is it open, where is its head, when did it end | No: the record is the directory's own entry | `line` |
| Read a line's facts at a cutoff: a client's history, a method's turn, a tool's recall, an index build, a report's evidence | No: the cutoff's validity, the atomic-batch boundary, the fixed head of an ended line and the continuation rule are this owner's knowledge | `read` |
| Read a line and its ancestors as one cut: a search over the chain, an index over the chain | No: each ancestor is bounded by the cut the line inherited from it, and the chain is the directory's knowledge | `read` with the chain scope |
| Read a line's purpose and source for its detail view | Yes: `line` carries the cut and source; the purpose is the parent's `branch/create` facts at that cut | Business composes with `line` and `read` |
| Learn whether a line is still open before writing to it | Yes: the record's state answers open, ending or ended | Business composes with `line` |
| Learn the configuration a line adopted at a cutoff | Yes for a live line: Prompt answers it through `line_owner`; for an ended line the facts are read at the fixed head | Business composes with `read`; Prompt keeps its own operation |
| Write an input to a line | Not this owner: the line's input owner decides order, idempotency and rejection | `line_owner::<InputQueue>` |
| Act on a line's own owner: compact it, read its usage, withdraw from it, approve in it, adopt a language or a profile in it | Not this owner: each is that owner's own operation | `line_owner::<T>` |
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
    fields: None,
    limit: None,
}).await?;

// A method that names a line reads its record first, then the same page.
let record = tree.line(requested.clone())?;
let page = tree.read(ReadLines {
    line: requested,
    scope: None,
    from: None,
    through: Some(record.head),
    fields: None,
    limit: None,
}).await?;

// session.branch_end: the client ends the line it opened.
tree.end(EndLine { line, source, final_message: None }).await?;

// Nothing is released here: the line id is a value and the record's head is a number.
```

A caller that needs the line's own owners — the input queue it writes into, the capability it compacts — calls no operation of this owner for that; the next section says how a Session part reaches those owners. The Session assembly drives the root line, the restore and the close through the owner's crate-private lifecycle, not through these operations, and no client or plugin may reach them.

## Session parts reaching a line's owners

A Session part acts on one line by line id, never through a `Branch` object. `line_owner::<T>(line)` resolves the branch-group service `T` of the instance the runtime holds for that line, and the caller calls that owner's own operation or reads its published value ([ADR 027](https://github.com/arcships/dim/pull/2029) D1). Any data face of the branch group may be reached this way, except the registration points (`sessionMethods`, `toolRegistry`, `promptRegistry`) and the lifecycle services (`timeline`, `dispatcher`, `branchLifecycle`, `timelineIdentity`); those stay inside the scope (ADR 027 D3).

`line_owner` answers with the service; a line that is not live (ended, or never this Session's) answers `NoInstance`, and a closing Session answers `Closed`. No operation name is a string, no request is a `json!` value, and no forwarding facade sits between the caller and the owner. The runtime keeps the `(session, line)` index it builds when it constructs a line's context, routes the call itself, and hands no instance key to the caller (ADR 027 D2); TimelineTree is not in this path.

Caller code after the change: a Session method steering a member line, a method reading a line owner's published value, and one using another data face of the line.

```rust
// Write the line's state: the member's input owner decides order and outcome.
line_owner::<InputQueue>(&member)?.submit(steer_input, cancelled).await?;

// Read a line's published value: the owner answers, not its timeline.
let usage = line_owner::<ContextUsage>(&line)?.read(None, cancelled).await?;

// Use any other data face of that line the same way.
let pending = line_owner::<Approval>(&line)?.pending().await?;
```

A Session part that cannot resolve a line keeps its business blocked; it does not fall back to holding a `Branch`, and TimelineTree adds no forwarding operation for it. [SessionScope](https://github.com/arcships/dim/pull/2093) documents the mechanism side with this contract.

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionScope` | `identity` for the Session id the store is keyed by, and the branch-construction entry (`openBranch`, target, pending [#2093](https://github.com/arcships/dim/pull/2093)) that builds a line's context; the tree holds and settles what that entry returns. |
| `S:dyn TimelineStore` | The line directory and a line's facts in storage, at a fixed head. |
| `S:GuideEntries` | The `<dim-branch-line>` and `<dim-branch-parent>` entries, until this part unloads. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors (codes) |
| --- | --- | --- | --- | --- | --- |
| `create` | Open a line at a cut of its parent | `{parent, cut, prompt, source, idleNotice?}`, `cancelled` | `{line}` | The caller's cancel stops only the wait for admission: a creation that was admitted builds and publishes, keeps its slot, and answers nothing to a caller that left; before admission nothing is reserved and the reply is `cancelled` | `invalid_parent`, `branch_ended` (the parent ended), `invalid_cut` (over the parent's head, or not an atomic-batch boundary), `branch_not_opener`, `branch_limit_reached`, `cancelled`, `closed` |
| `end` | End a line and settle it | `{line, source, finalMessage?}` | `{}` | None: an admitted end settles even if the caller goes away | `unknown`, `branch_cannot_end` (the root never ends; the conversation line and its ancestors need a move first), `branch_target_protected` (the target or an ancestor is ending), `final_undelivered` (the line's own end is recorded and its final message is still owed; the call retries delivery only and answers this until the message lands), `branch_ended` (the line is ended and owed nothing), `closed` |
| `set_default` | Move the conversation to a line | `{line, source}` | `{default}` | None: the move is one commit | `unknown`, `branch_ended` (the target ended), `branch_already_default`, `branch_not_selectable` (a plugin line), `branch_target_protected` (the target or an ancestor is ending), `closed` |
| `directory` | The lines and the current default, from one read | `{}` | `{default, lines}` | None | `closed` |
| `line` | One line's record | `{line}` | `record` | None | `unknown` (no such line, or still a reservation), `closed` |
| `read` | A line's facts at a cutoff, or the line and its ancestors at one cut | `{line, scope?, from?, through?, fields?, limit?}` | `{through, events}` or `{through, rows}` | None: one bounded read | `unknown`, `beyond_head` (a cutoff past the line's head), `not_batch_boundary` (a cutoff inside an atomic batch), `continuation_without_cutoff`, `closed` |

A record is `{line, parent, cut, source, state, head, endEvent?}`; `state` is `open`, `ending` or `ended`. A line whose own `branch/end` is recorded is `ending` until its scope closed and its final message, if any, is delivered; it is `ended` once both are done, and only then does `end` answer `branch_ended` instead of `final_undelivered`. A reservation has no record yet and answers `unknown` until it is published. `read` answers a live line from its scope and an ended line from the store at its fixed head, with the same shape and the same visibility. The chain scope reads only published facts, bounds every ancestor at the cut the line inherited, and keeps the original cutoff across a continuation page. `fields` absent selects the whole event data; ids and sizes cross as strings and numbers; no handle, cursor or channel crosses.

### Errors

| Kind (wire code) | What the caller does next |
| --- | --- |
| `invalid_parent` | Retry with an existing line, after reading `directory` |
| `branch_ended` | The line is gone and owed nothing: read it with `read`, or pick a live line. Not retryable |
| `invalid_cut` | Retry with the parent's head from `line`, or a cutoff that ends an atomic batch |
| `branch_not_opener` | Not retryable from this line: only the root and the conversation line open lines; move the conversation first with `set_default` |
| `branch_limit_reached` | Retry after ending an open tool line |
| `branch_cannot_end` | Not retryable as is: the root never ends; for the conversation line or one of its ancestors, move the conversation first and call again |
| `branch_target_protected` | Stop and do not retry the same call: an end already on its way to the target or an ancestor wins; read `line` and act on the state found |
| `branch_already_default` | Not retryable and nothing happened |
| `branch_not_selectable` | Not retryable: a plugin line cannot be the conversation line |
| `final_undelivered` | The line's end is recorded and its message is still owed; call `end` again (retryable) to deliver it. Until it lands the record reads `ending`, not `ended`. The message never delivers twice |
| `beyond_head` | Retry with the record's head as the cutoff |
| `not_batch_boundary` | Retry with a reply's `through`, or the head of a record |
| `continuation_without_cutoff` | Retry with the original `through` on the continuation page |
| `unknown` | Not retryable with the same id: read `directory` for the lines in force |
| `cancelled` | Retryable. Before admission nothing happened; after admission the line is being built and publishes without a waiter, so read `line` before opening again |
| `closed` | Not retryable in this generation: the Session is closing or unloading; a follow-up decision waits for the Session's replacement |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Validate parent, cut, opener and cap under one lock, reserve the line, build the scope, write the opening blocks and the first input, publish; a caller that stopped waiting leaves the build running and the slot spent | Before admission: nothing reserved, `cancelled`. After admission: the build continues, the line publishes and charges nothing again; a definitively failed creation writes `branch/create-failed` and releases the slot | The admitted build continues; the line is the Session's, not the caller's | A build in flight finishes or is rolled back before the generation closes; new calls answer `closed` | TimelineTree: the scope, the reservation, the slot and a failed creation's fact |
| `end` | Check the target, end descendants deepest first, commit `branch/end` (a tool end carries its final message), deliver that message to the parent, close the scope, write `branch/ended` on the parent | None; the call answers when the end settles | The end continues; the caller's absence changes nothing | An end in flight finishes before the generation closes; the close waits for it and for the store's answer | TimelineTree: the delivery, the scope's close and the end facts |
| `set_default` | Validate the target and its chain, write `branch/default` on the root, queue one transition per participant, wake the writers | None | Nothing to release | Refused with `closed` once closing starts | TimelineTree records the transitions; each participant's BranchLine writes its own note |
| `directory` | Read the default and every record under one lock, resolve each head | None | Nothing to release | Pending reads answer at the state they observed | TimelineTree |
| `line` | Read one record under the tree lock | None | Nothing to release | An ended line answers until its record leaves; an id the Session never had answers `unknown` | TimelineTree |
| `read` | Resolve the line and the cutoff, check the batch boundary, read the facts, answer with the cutoff | None | Nothing to release | An ended line's read answers from the store; a live line's read answers `closed` once its scope is gone | TimelineTree |

## Exceptions with reasons

- A child's opening blocks are written before the child's scope is published: until publication the line has no other owner, so no other owner's timeline is written.
- `end` delivers a tool end's final message into the parent's timeline: the message is part of the end's settlement, and the accepted design keeps the delivery with the end so `branch/end` carries the message and restore can finish exactly once ([timeline branches §10](../../design/timeline-branches.md#10-ending-a-line)). Delivery happens before the scope close and before `branch/ended`, so a parent that holds `branch/ended` is never owed a message.
- `read` resolves a live line through the directory and returns its facts unchanged: facts are shared state under design rules §4, not another owner's folded state.
- An ended line's facts are answered here because the line's own owners are gone with its scope; the tree keeps the fixed head and the directory record.
- `set_default` queues one transition per participant: the note's key and text are BranchLine's, and only the transition is recorded here, in commit order, because a restart must not lose it.
- The owner's branch parts hold a weak handle to this owner (`TreeHandle`, holding `Weak<TimelineTree>`): the tree holds each line's scope and the scope holds the part, so a strong handle would be a cycle and close could not settle the scope. The handle is built when the tree is created, answers `closed` once the tree is gone, and is not a public face.

## Verification

Flow tests on the owner:

- `create` charges opener, cap and reservations under the same lock: a second create and a `set_default` interleaved with it cannot both pass a check; a caller that cancels after admission leaves a line that publishes, and a definitively failed creation releases its slot and writes `branch/create-failed`.
- `end` ends descendants before their parent, records one `branch/ended` each, writes `branch/ended` only after the final message is delivered, and refuses the root and the conversation line's chain with their own kinds.
- A tool end whose final message cannot be delivered answers `final_undelivered`; a later `end` or a restore delivers it exactly once.
- `directory` answers `default` and the lines from one read under a concurrent move or end; `line` answers an open, an ending and an ended line, and `unknown` for a reservation or an id the Session never had.
- `read` answers an ended line at its fixed head with the same field selection as the live line, refuses a cutoff inside an atomic batch, and keeps the original cutoff across a continuation page.
- Close cancels reservations, joins builds, closes every live scope and answers after the store's last batch; a late `TreeHandle` use answers `closed`.

## Supersedes

- The `timelineTree` rows of [runtime owner operations](../../design/contracts/runtime-owner-operations.md), its `timelineTree.call` operation and its "Calls from a Session to a selected branch" section: the tree's face is this contract: a Session part resolves a branch-group service by line id (`line_owner`) and no forwarding call exists.
- [Service faces §3.3](../../design/service-faces.md) row for `treeCommands` and `timelineTreeControl`: both names are gone; `timelineTree` is this owner's single data face.
- The migration note at the top of [timeline branches](../../design/timeline-branches.md), and its binding table row for the TypeScript `TreeCommands`: `treeCommands`, `TreeCommands` and `TimelineTreeControl` name the pre-redesign binding and no longer exist. The accepted behavior in that document — the cap's accounting, the end's order and the final message's delivery — is unchanged.
- [Plugin contract](../../design/plugin-contract.md) rows naming `treeCommands` as the Session-to-branch path and the mechanism-service row of the Session instance: a Session part reaches a line's owners with `line_owner::<T>(line)` ([ADR 027](https://github.com/arcships/dim/pull/2029)).

## Client-visible changes

None to names, fields or error codes: `session.branches`, `session.branch_create`, `session.branch_end`, `session.branch_set_default`, `session.branch.get`, `session.history` and `session.model.get` keep their method names, request and reply fields, and the `branch_*` codes. The App Server maps the kinds above onto the codes it answers today.

One narrowing is visible: a line appears in `session.branches` and answers `session.branch.get` once it is published, not while it is a reservation, because a reservation has no head or state. A client observes a line after `session.branch_create` answers, which is after publication.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | TimelineTree is one Session part; TreeTimeline and BranchLine are its two branch parts; BranchTool is a separate Session part (its own document). TimelineTree writes Timeline facts and folds no state from them: not a Capability. TreeTimeline and BranchLine are Capabilities: they consume a line's committed facts, and the tree writes the facts that follow. |
| §2 scopes | Session scope for the directory, the lifecycle and the weak handle; branch scope for TreeTimeline and BranchLine. The Session owns the child scope's lifecycle under ADR 002 D1. |
| §3 orthogonality | No other owner's state is read or changed. The final message is delivered by the end, as the accepted design assigns it; the default line's notes are written by each participant's BranchLine. |
| §4 rule 1 | No command event from a client: `create`, `end` and `set_default` write facts only after their admission commits. |
| §4 rule 2 | The end and the move hand off through facts (`branch/end`, `branch/ended`, `branch/default`); each participant's BranchLine drains its transition and writes its note. |
| §4 rule 3 | No handler waits for I/O: creation and settlement run in the owner's tasks, and close joins them. |
| §4 rule 4 | The directory and the facts are the Session's own state; no other owner's record is copied into them. |
| §4 rule 5 | Batch boundaries are enforced by `read` and by `create`'s cut. |
| §4 rule 6 | No event reaches the model from here; the opening blocks are inputs the tree writes, and the model reads them as inputs. |
| §4 rule 7 | Callers use these six operations; no caller reads a node, a queue or a scope. |
| §4 rule 8 | One requirement is raised below: resolving a line id to a branch-scope service. |
| §4 rule 9 | Facts are published after their atomic batch commits; the end and its message are one fact. |
| §7 | Every argument and reply is data and a line is an id; no object or handle crosses, except the owner's internal weak handle. Deleting the old surface and migrating every caller is one change. The owner answers its own state and facts, never another owner's record. |
| ADR 002 D1 | Session scope: the directory outlives every line and every branch scope is created and settled here. |
| ADR 002 D2 | No wiring plugin: the service is on `TimelineTree`; the weak handle is an internal reference, not a public face. |
| ADR 002 D3 | Dependencies point from extensions to the core; TimelineTree depends on the Session scope, the store and the guide entries only. |
| ADR 002 D4 | No setting selects a behaviour here; the cap BranchTool configures is charged by this owner. |
| ADR 002 D5 | The contract is language-neutral: ids, records and facts; the Rust binding is the appendix. |
| ADR 027 | D1: a Session part reaches a branch owner with `line_owner::<T>(line)`. D2: the runtime routes by id and hands no instance key to the caller. D3: any data face except the registration points and the lifecycle services. |

## Requirements raised to mechanism owners

1. **Resolve a line id to a branch-scope service.** `line_owner::<T>(line)` is the runtime's id routing ([ADR 027](https://github.com/arcships/dim/pull/2029) D1–D3), owned and tracked in [#2127](https://github.com/arcships/dim/issues/2127): resolve the line to the live instance, run the target owner's own operation there, and answer `NoInstance` when the line is not live or `Closed` when the Session is closing. The mechanism keeps the index and hands no instance key to the caller; TimelineTree stays out of the path and adds no forwarding operation. Session-side use is stated in [SessionScope](https://github.com/arcships/dim/pull/2093) ([#2025](https://github.com/arcships/dim/issues/2025)).

2. **The Session access surface stops carrying line operations.** `Session::end_branch`, `set_default_branch`, `branch`, `branches`, `directory`, `default_and_directory`, `ended_head`, `sequence`, `batch_end` and `query` forward here today; after this contract the tree is the only face and the Session surface must not restate it. [SessionScope](https://github.com/arcships/dim/pull/2093) owns that surface.

## Binding appendix

The service is marked on `TimelineTree`; the wire name is `timelineTree`. `TreeCommands`, `TimelineTreeControl` and the `Branch` facade no longer cross this owner's boundary. `CancelledFn` is the crate's existing cancellation argument and is not a request field. Requests carry serde data only.

```rust
use crate::host::CancelledFn;
use dim_agent_types::BranchSource;
use rutis::service;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRecord {
    pub line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<BranchSource>,
    pub state: LineState,
    pub head: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_event: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineState {
    Open,
    Ending,
    Ended,
}

/// The idle-notice policy a new line's opener chooses; the wire value is `steer` or `none`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdleNotice {
    #[default]
    Steer,
    None,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLine {
    pub parent: String,
    pub cut: u64,
    pub prompt: String,
    pub source: BranchSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_notice: Option<IdleNotice>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndLine {
    pub line: String,
    pub source: BranchSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub default: String,
    pub lines: Vec<LineRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineScope {
    Line,
    Chain,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadLines {
    pub line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<LineScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// One or the other: an event page, or field rows.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ReadReply {
    Events { through: u64, events: Vec<dim_agent_types::AgentEvent> },
    Rows { through: u64, rows: Vec<dim_agent_runtime::QueryRow> },
}

/// Every kind the owner answers, tagged by the wire code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LineError {
    #[error("no such line: {line}")]
    InvalidParent { line: String },
    #[error("line {line} ended")]
    BranchEnded { line: String },
    #[error("cut {cut} is not usable on line {line}")]
    InvalidCut { line: String, cut: u64 },
    #[error("line {line} may not open a line")]
    BranchNotOpener { line: String },
    #[error("the Session holds its limit of open lines")]
    BranchLimitReached,
    #[error("line {line} cannot end")]
    BranchCannotEnd { line: String },
    #[error("line {line} or its chain is ending")]
    BranchTargetProtected { line: String },
    #[error("line {line} is the conversation line")]
    BranchAlreadyDefault { line: String },
    #[error("line {line} cannot be the conversation line")]
    BranchNotSelectable { line: String },
    #[error("line {line} ended but its final message is not delivered")]
    FinalUndelivered { line: String },
    #[error("cutoff {cutoff} is past line {line}'s head {head}")]
    BeyondHead { line: String, head: u64, cutoff: u64 },
    #[error("cutoff {cutoff} is inside an atomic batch")]
    NotBatchBoundary { cutoff: u64 },
    #[error("a continuation needs the original cutoff")]
    ContinuationWithoutCutoff,
    #[error("unknown line: {line}")]
    Unknown { line: String },
    #[error("the call was cancelled before admission")]
    Cancelled,
    #[error("the Session is closing")]
    Closed,
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

`LineError`'s serde tag is the wire `code`; the variant names match the errors table one for one (`BranchTargetProtected` answers `branch_target_protected`, and so on). Records, requests and replies are camelCase on the wire; `cut`, `through` and `head` are unsigned integers; a line id is a string. `TreeHandle` is the owner's internal weak reference to `TimelineTree`; it is not a service and has no wire name.
