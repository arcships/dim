# Goal

Scope: branch state with a Session entry. Issue: #2033 (this contract); #1320 (the goal feature). Evaluator judgement is a mode of this owner, described in [Pursuit](Pursuit.md); the state object and its reload mapping are in [Goal-state-read.md](Goal-state-read.md), and the two places where this contract supersedes that document are named below.

Goal owns the goal of one line: the objective, the judge, the status and the reason it changed, the decisions that follow, and the model-visible texts that state them. A Session part registers the commands; a branch part holds the state. The current shape is the one #2033's scan report describes.

## Use and scope: responsibility split

Goal owns:

- **The goal of a line.** One objective, the judge that decides completion, the status (`active`, `paused` with a reason, `completed`, `blocked`, or none) and the moment it last changed. The state belongs to the line and ends with it.
- **The decisions that follow a fact.** After a turn ends: continue, stop at the optional continuation limit, stop after three turns without progress, or ask for an evaluation. On an interrupt, a failed or cancelled turn, or an unload: pause with the reason. When a child line starts: retire the keys of an inherited goal.
- **The texts the model reads.** The `dim-goal` marker's command block, state block and continuation, and the guide entry that explains them.
- **The commands' vocabulary.** `set`, `edit`, `pause`, `resume`, `clear` and the completion report, their parameter rules, and the two entry points that carry them (`session.goal.*` and the `goal_update` tool). The retained limits: an objective of 1..=4000 characters, a summary of 1..=2000, and `maxContinuations` in 1..=1000 with absence meaning no limit.
- **The evaluator line's purpose.** Goal asks TimelineTree to open a child line cut at the turn end and closes it with the operation id it wrote.

Callers keep as business:

- Which line a command names; a client that names none means the conversation line.
- Which command and its parameters; the operator's presentation of the goal.
- History: a client that shows the goal of an ended line reads that line's facts and folds them itself.

| Requirement (caller) | Can callers compose it from the owner's operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client submits a goal command for a line | No: the command must reach the line's input owner in order, and the resulting state is the owner's | the Session entry writes one command input to the line |
| The model reports completion or blockage | No: the report changes the goal, and its line is the call's line | the tool handler writes one command input to that line |
| A client reads the goal of a line | No: the goal is the line owner's state and nothing else publishes it | `goalLine.read` at the line id |
| The owner states the state to the model | No: the state block restates the whole goal and the live keys when it changes | the branch owner writes the state record input |
| A command is retried after a lost reply | No: accepting a command once is the input owner's rule | `submissionId` on the request when the caller retries; Goal generates one when absent |
| A client shows the goal of an ended line | Yes: the line's facts stay readable at its fixed head and the fold is the client's projection | business composes with TimelineTree's `read` |
| A client reads the conversation line's goal without naming it | Yes: the line record carries the Session's current line | business composes with TimelineTree's `directory` |
| The owner continues a goal after a turn | No: the decision uses the owner's fold, counters and tasks | branch-internal |

An operation or parameter exists only where its row says No.

## Caller example

`session.goal.set` writes one command input to the named line; the branch owner decides and states the result. The reply carries the committed command input's sequence, the same value the method returns today.

```rust
// The entry checks its own parameters and resolves the line a client left unnamed.
let line = match command.line {
    Some(line) => line,
    None => tree.directory()?.default,      // TimelineTree's shape (#2092)
};
let _ = tree.line(line.clone())?;   // the tree's `unknown` kind; the method answers session_invalid_input
// Goal's own text: the command block carries the operation, the objective and the judge.
let input = goal_command(&line, &command);
line_owner::<InputQueue>(&line)?
    .submit(input, cancelled)
    .await?;
// The reply is the committed command input's sequence; nothing else is returned and nothing is released.
```

`session.goal.get` reads the line owner's published value; the caller decides what "no owner" means for its screen.

```rust
let goal = match line_owner::<GoalLine>(&line) {
    Ok(owner) => owner.read().await?,
    Err(NoLine) => return Err(AgentError::Reloading("Goal".into())),       // service_reloading
    Err(SessionClosing) => return Err(AgentError::Closed),                  // session_closed
    // NoSession cannot arrive here: only a process caller receives it.
};
// `goal.objective`, `goal.status`, `goal.evaluator`, `goal.through`; history is read from facts instead.
```

`goal_update` writes a report input to the call's line:

```rust
let line = execution.line.clone();          // the call's line, a value
line_owner::<InputQueue>(&line)?
    .submit(goal_report(&line, status, summary), cancelled)
    .await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| TimelineTree ([#2024](https://github.com/arcships/dim/issues/2024)) | `directory`, `line`, `read`, `create`, `end` | The conversation line; one line's record for the named line; an ended line's facts; opening and closing the evaluator line |
| The line's input owner ([#2027](https://github.com/arcships/dim/issues/2027), target, pending [#2102](https://github.com/arcships/dim/pull/2102)) | `submit` | The command input only |
| The line's timeline (branch runtime) | `append` | The business fact recovery reads, then the state record input |
| Dispatcher, BranchLifecycle, TimelineIdentity (branch runtime) | facts, activation, closing | Folding the line's facts and the branch part's lifetime |
| SessionScope ([#2025](https://github.com/arcships/dim/issues/2025)) | `phase` | Not ending evaluator lines while the Session is closing |
| Settings ([#2017](https://github.com/arcships/dim/issues/2017)) | `register`, `read` | The `goal` source's optional `maxContinuations` |
| Session registries ([#2026](https://github.com/arcships/dim/issues/2026)) | `sessionMethods.register`, `toolRegistry.register`, `guideEntries.register` | The commands, the tool and the guide entry |

The line-id operations that `line_owner` names are the ones the cross-cutting decision puts in TimelineTree's contract ([#2024](https://github.com/arcships/dim/issues/2024)); SessionScope's contract has no such operation.

### Operations

The entry's operations are the five commands, the read, the tool and the guide entry; the branch owner's operation is one read.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `session.goal.set` | Start a new execution of a goal on a line | `{sessionId, branchId?, objective, judge?, submissionId?}` | `{sequence}`: the committed command input's sequence, as today | Before the input is submitted | `invalid_params`, `session_invalid_input` (no such line), `branch_ended`, `service_reloading`, `session_closed`, `operation_interrupted` |
| `session.goal.edit` | Replace the objective, keeping status and judge | `{sessionId, branchId?, objective, submissionId?}` | `{sequence}` | Before the input is submitted | `invalid_params`, `session_invalid_input` (no such line), `branch_ended`, `service_reloading`, `session_closed`, `operation_interrupted` |
| `session.goal.pause` / `resume` / `clear` | Pause, resume or clear the goal | `{sessionId, branchId?, submissionId?}` | `{sequence}` | Before the input is submitted | `invalid_params`, `session_invalid_input` (no such line), `branch_ended`, `service_reloading`, `session_closed`, `operation_interrupted` |
| `session.goal.get` | The goal of a line now | `{sessionId, branchId?}` | `{branchId, through, goal?}`; the goal carries `{objective, status, evaluator}` | The wait for the owner's fold stops | `invalid_params`, `session_invalid_input` (no such line), `service_reloading` (also for an ended line), `session_closed`, `operation_interrupted` |
| `goal_update` | The model reports completion or blockage | `{status, summary}` | text in `ToolResult.content` | Before the input is submitted | the commands' codes, returned as an error result |
| `goalLine.read` | The line's published goal value | `{}` | `{objective?, status, evaluator, through}` | The wait stops; the value is unaffected | `closed` (the service's `{code, message}`) |

`invalid_params` is Goal's own parameter check: an objective outside 1..=4000 characters, a summary outside 1..=2000, an unknown judge, an empty `branchId`. `session_invalid_input` is the published code for TimelineTree's `unknown` kind and for `AgentError::InvalidInput`, and `session_closed` for `AgentError::Closed` (`crates/dim-agent-capabilities/src/methods.rs:137-154`); `branch_ended` is the ended-line refusal today, also for a route that answers `NoLine` after the record was read, and the input owner's contract names it `line_ended` (target, pending [#2102](https://github.com/arcships/dim/pull/2102)), which lands as a client-visible code when that owner ships; `service_reloading` is the mechanism's answer that the line has no live goal owner, which the client shows as no goal and reads from history instead. The entry keeps the mechanism's three answers apart ([#2127](https://github.com/arcships/dim/issues/2127)): `NoLine` and a reloading owner are `service_reloading`, `SessionClosing` is `session_closed`, and `NoSession` never reaches a Session part. Every code above is one a client receives today.

`goalLine.read` takes no request parameter: the line is the instance the mechanism resolved. Requests, replies and errors are data; `cancelled` is the binding's cancellation argument.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `session.goal.*` write | Check parameters, resolve the line, compose the command input, submit it; the input owner commits it in order | Before submission nothing is written; after submission the input stays and the branch owner decides | Nothing to release: no id crossed | `closed` once the Session or the input owner is closing | the input owner; the entry holds nothing |
| `goalLine.read` | Wait for the branch part's fold to reach its head, answer the value | The wait stops; the value is unaffected | Nothing to release | `closed` when the part is closing | the branch part |
| Branch decision | Consume a command input, decide from the fold, append the business fact, append the state record, then write the continuation or ask for an evaluation | The line's closers end the branch part's tasks; an admitted input is not withdrawn | — | Closing cancels the tasks, writes `paused` with its reason, then the services are withdrawn | the branch part: its own tasks, the fact and the record; the input owner: the continuation it accepted |
| Evaluator line | Open a child line at the turn end with Goal's operation id; the evaluator reports a conclusion; Goal closes the line | The line's close stops the decision that would open it | — | On rebuild and on close Goal closes the evaluator lines it opened, before the Session closes | the branch part asks; TimelineTree creates, holds and closes the line |
| Evaluator line refused | `create` answers `branch_not_opener` or `branch_limit_reached`: Goal pauses the goal with reason `evaluation_failed` and writes no conclusion | — | — | — | the branch part; the parent line's fact carries the pause |

The business fact precedes the state record: the fact is what recovery reads, the record is model-visible text. A crash between the two leaves the goal correct in recovery and one state block missing from history; the next change restates the whole state. This supersedes [Goal-state-read.md](Goal-state-read.md)'s one-batch command write; the state object, the load mapping and the property queries in that document stay.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_params` | Fix the parameters: the objective, the judge, the summary, the line id; retry with the fix |
| `session_invalid_input` | The named line does not exist; read the directory and choose a line, then retry |
| `branch_ended` | The line has ended; read its facts for history or choose an open line |
| `service_reloading` | The id routing answered `NoLine` (the line is closing, ended or not this Session's) or the goal owner is reloading; show no goal, read history from the line's facts, retry later |
| `session_closed` | The Session or the input owner is unloading; the next generation serves the next Session |
| `operation_interrupted` | The mechanism interrupted the call on unload; the accepted input is settled, the read stops waiting |

## Exceptions with reasons

- The evaluator line is created with TimelineTree's `create` and closed by id: the purpose is Goal's, the line and its scope are TimelineTree's, and no handle to the child scope crosses.
- Goal keeps a model-text protocol and folds the record it committed: the command block, the state block and the continuation are Goal's vocabulary, and a reader of history that needs the objective decodes the state block.
- Goal does not answer for an ended line: folding that line's facts in the Session entry would make the entry a second owner of branch state, which the Session access surface forbids.
- The state record follows the business fact instead of sharing one batch: the fact is authoritative, the record is derivable text, and a crash between them loses text rather than state.
- A `before` hook that rewrites Goal's state block is outside Hooks' contract: Goal folds what it committed, so the model and Goal cannot disagree about what the model read.

## Verification

Flow tests through the entry's public interfaces and the branch owner:

- `set` writes one input to the named line; the branch owner states `active` with the objective; `get` answers the same value while the line is live.
- `pause`, `resume` and `clear` change the status in the stated order; a paused goal does not continue; `clear` retires the model-visible keys.
- `get` on an ended line answers `service_reloading`, and the line's facts still carry the last state for a client that folds them.
- A turn end continues, stops at the configured limit, and stops after three turns without progress; an interrupt, a failed turn or an unload writes `paused` with its reason.
- An evaluator goal opens one child line per turn end and closes the line it opened on rebuild and on close; a conclusion sets `completed` or `blocked`; a refused `create` (`branch_not_opener`, `branch_limit_reached`) pauses the goal with `evaluation_failed`.
- A dropped read leaves nothing behind; a command submitted twice with one submission id is accepted once.

## Supersedes

- [Goal-state-read.md](Goal-state-read.md): the one-batch command write and its ordering rule. The state object, the load mapping and the property queries in that document stay; the record now follows the business fact.
- [runtime-owner-operations.md](../contracts/runtime-owner-operations.md): the Goal rows — `inputs(id, inputs)` as one atomic record-and-trigger batch, and `directory({id})` returning one branch's record. Goal writes one command input through the input owner and reads `directory()`/`line`.
- The Chinese mirror `docs/zh/design/plugins/Goal.md` describes the pre-contract shape; its target is this document.
- The `session.goal.*` reply field `sequence` stays; the routing table, the Session-side re-fold and the pre-commit callback are deleted.

## Client-visible changes

- `session.goal.*` replies keep their fields: `{sequence}` is the committed command input's sequence, the same value returned today; `session.goal.get` keeps `{branchId, through, goal?}` with `{objective, status, evaluator}`. Every error code above is one the client already receives. The write requests gain an optional `submissionId`; a client that sends none keeps today's behavior, and a retrying client sends the same id to be accepted once.
- An ended line no longer answers `session.goal.get` with its last goal: the entry has no live owner to read, and folding the line's facts in the entry is what this contract removes. `session.goal.get` answers `service_reloading` and a client shows no goal. Affected: `dim-cli`'s `/goal` and `sessions.status` projection (`crates/dim-cli/src/goal_command.rs`, `session_host.rs`) and the TUI's goal panel, which read the value. Migration: read the line's facts (`goal/state`) for an ended line, or show no goal; the CLI change ships with the implementation.
- The `goal:state` record no longer shares one batch with the command. The model sees the command input first and the state record from the next request; the record's text format is unchanged. Affected: the prompt-evaluation tests that assert the command and record order, and `Goal-state-read.md`'s ordering rule.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | `GoalBranch` injects the Dispatcher and the Timeline, writes and folds facts, so it is a Capability. `GoalSession` registers a tool, a method namespace and a guide entry: an entry point, not a Capability. |
| §2 scopes | The goal belongs to the line; the commands belong to the Session entry; nothing about the goal is held at the process scope. |
| §3 orthogonality | Goal reads TimelineTree's line record and its own facts, writes through the input owner and its own timeline, and returns no other owner's data. |
| §4 rule 1 (commands) | A client's command is an input to the line's input owner; Goal never writes another owner's state. |
| §4 rule 2 (handover) | The evaluator line is opened by TimelineTree and closed by id; no branch scope or object crosses. |
| §4 rule 3 (before callbacks) | Goal registers no `before` callback and folds what it committed. |
| §4 rule 4 (no business meaning in core) | The marker, the state block and the guide entry are Goal's; the core carries none of them. |
| §4 rule 5 (batches) | The fact and the record are separate appends in that order, so recovery never reads state that was not committed. |
| §4 rule 6 (context) | The model reads goals only through adopted inputs; `goalLine.read` serves callers, not context preparation. |
| §4 rule 7 (public events and services) | The branch part uses the Dispatcher, its timeline, TimelineTree and the input owner; the entry uses the registration points and the mechanism. |
| §4 rule 8 (requirements) | Three requirements are listed below; no core change. |
| §4 rule 9 (facts) | The goal is a line fact plus the model-visible record; execution counters stay in memory. |
| §7 values, ids and callbacks | The entry writes an input (data) and reads a value at a line id; the mechanism resolves the line's owner; no object crosses. |
| §7 no proxy lookup | The entry neither folds the line's state nor answers from Goal's facts; the branch owner answers its own value. |
| §7 one interface | The commands are one method namespace and one tool contribution; the branch owner's face is one read; no adapter exists beside either. |
| §7 cross-language | Commands, parameters, replies and errors are data; the tool result is text in `ToolResult.content`. |
| §7 one change | The implementation task deletes the routing table, the re-fold path and the pre-commit callback in the same change that adds `goalLine.read` and migrates the entries. |
| ADR 027 | Applies: D1 and D4. The mechanism is [#2127](https://github.com/arcships/dim/issues/2127). |
| ADR 027 D3 | No registration point and no lifecycle service is resolved by line id: `GoalLine` and `InputQueue` are data faces of the branch group; the registration points it uses (`sessionMethods`, `toolRegistry`, `guideEntries`) are its own Session's. |
| ADR 002 D1 | The goal, its decisions and its tasks live in the branch; the Session entry registers the commands. |
| ADR 002 D2 | No wiring plugin: the branch part provides `goalLine` on its own type; the mechanism resolves the line. |
| ADR 002 D3 | Dependencies point from Goal to TimelineTree, the input owner, Settings and the runtime; nothing depends on Goal. |
| ADR 002 D4 | `maxContinuations` is business configuration read at use; no reload. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

- [TimelineTree #2024](https://github.com/arcships/dim/issues/2024): the cross-cutting decision names this owner's contract as the place that fixes who provides the line-id operations to Session parts. Goal needs two: a Session part resolving `GoalLine` for the read and the line's `InputQueue` for the command. SessionScope's contract has no such operation.
- [The line's input owner #2027](https://github.com/arcships/dim/issues/2027): `submit` accepts one validated input with its submission id, commits it in order and answers its position; an ended line and a closing owner are data.
- The line's timeline owner (runtime, outside the #2015 scan): `append` accepts an input-message draft and an extension fact; Goal appends the fact and then the record. The record is an input-message draft, never a second `submit`.
- The id routing mechanism is [#2127](https://github.com/arcships/dim/issues/2127); the `line_owner` requirement lands there, and #2024's contract states the line-id operations it serves ([target, pending #2092](https://github.com/arcships/dim/pull/2092)).


## Compatibility anchors

These headings exist only so the links in other documents and code comments resolve; their content is in the sections above.

### 2. Data and state

The goal belongs to a line; the state object and its load mapping are in [Goal-state-read.md](Goal-state-read.md), and [Own contract](#own-contract) states the write order.

### 3.4 A new branch clears the inherited goal

A child line starts with no goal; see the decisions in [Own contract](#own-contract) and [Exceptions with reasons](#exceptions-with-reasons).

### 5. Context mapping

The model reads goals only through adopted inputs; see [Own contract](#own-contract) and [Client-visible changes](#client-visible-changes).

### 10. Implementation

The implementation order and the deletion list are in #2033's delivery comment.

### A.2 Session methods

The `session.goal.*` parameters, replies and errors are in [Own contract](#own-contract).

### A.3 Reading the goal

See `session.goal.get` and `goalLine.read` in [Own contract](#own-contract); an ended line no longer answers.

### A.4 Continuation decision

Continuation, the limit and no-progress are in [Own contract](#own-contract).

### A.5 Model-visible texts

The marker, the state block, the command block and the continuation remain Goal's model protocol; the text format is unchanged, and the record's order is in [Client-visible changes](#client-visible-changes).

### A.5.1 Protocol and policy

The guide entry is in [Own contract](#own-contract); the policy sentences stay Goal's.

### A.5.2 Evaluation

The evaluator line and its conclusion are in [Own contract](#own-contract) and [Exceptions with reasons](#exceptions-with-reasons).

### Appendix A. Interface details

The command and read details are in [Own contract](#own-contract).

### Appendix B. Full conflict table

Conflicts and termination are in [Own contract](#own-contract)'s Flows.

### Appendix E. Assumptions

The parameter limits are in [Use and scope: responsibility split](#use-and-scope-responsibility-split).

## Binding appendix

The branch owner's face is marked on its own type and provided inside the line's scope; the mechanism resolves it by line id. The entry uses the existing registration points. `cancelled` is the binding's cancellation argument, not a request field.

```rust
use rutis::host::BoxFuture;

/// The value the branch part publishes; `through` is the head the fold reached.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalValue {
    pub objective: Option<String>,
    pub status: GoalViewStatus,      // dim_agent_types::GoalViewStatus: active | paused{reason} | completed | blocked | none
    pub evaluator: bool,             // the judge; `GoalView` uses this field name on the wire
    pub through: u64,
}

#[rutis::service]
impl GoalLine {
    /// The goal of this line now; waits until the fold reaches the head.
    pub async fn read(&self, cancelled: CancelledFn) -> Result<GoalValue, ServiceError>;
}

/// The service error shape of every data face: `{code, message, data?}`.
#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct ServiceError {
    pub code: String,                // "closed"
    pub message: String,
}
```

`GoalViewStatus` and `GoalView` are the existing types in `dim_agent_types`; the command input's text and its `dim-goal` marker are the existing ones in `crates/dim-goal`, and their wire spelling is unchanged.
