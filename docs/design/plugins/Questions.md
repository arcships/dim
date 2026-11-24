# Questions

Scope: session. Issue: #2026.

Questions owns the `ask_user` tool and the Session part that holds the model's pending questions: the table keyed by `(line, request, call)` and the claim that makes a question visible before its call is published. The current implementation is in `crates/dim-agent-questions/src/lib.rs`; the plugin page is [Questions](../../architecture/rust-agent/plugins/Questions.md). The claim's tool-side contract is in [ToolRegistry](ToolRegistry.md#own-contract).

## Use and scope: responsibility split

Questions owns:

- **The waiter table.** One entry per waiting question, keyed by line, request and call; whoever removes the entry under the lock decides the call's output.
- **The answer's validation.** Answers must name every question once, choose offered labels and respect `multiSelect` and `allowCustom`.
- **The claim.** A question exists before its `step/tool-call` fact and is removed when the answer, the call's cancel or the part's unload ends it.

Callers keep:

- When to ask: the model decides; the tool's input shape is this owner's declaration.
- Whether a question may run: Permission and the Session's `interactive` flag decide.
- The client's answer: it arrives as the `session.question.respond` method, not as a fact.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model asks and waits for one answer | No: the table, the claim and the single-answer rule are the owner's | the `ask_user` tool |
| A client answers or declines the waiting call | No: the entry and the answer validation are the owner's | `session.question.respond` |
| A client lists the waiting questions on a line | No: the table is the owner's | `session.question.pending` |
| The question is visible before the fact that publishes it | No: the claim must exist before the append and be released on every path that never dispatches | the claim callback |

An operation or parameter exists only where the row says No.

## Caller example

The tool registers with its declaration and claim callback; the claim inserts the waiter and returns nothing:

```rust
tools.register(
    ctx,
    ask_user_definition(),
    |call, context, cancelled| Box::pin(run_ask_user(call, context, cancelled)),
    |call, context| Box::pin(claim_question(call, context)),
).await?;
```

A client answers; the method resolves the waiter and the call's result follows:

```rust
// The `session.question.respond` handler resolves the waiter; it answers {} once the waiting call takes it.
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:sessionScope` | `config` for `interactive`, read per call; a non-interactive Session gets an error result. |
| `S:toolRegistry` | Register `ask_user` until this part unloads. |
| `S:sessionMethods` | Register `session.question` until this part unloads. |
| `S:timelineTree` | `query` for the pending read at the line's cutoff. |

### Contributions

No service of its own. The table is reached only through the tool call's wait and the two methods.

| Contribution | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- |
| `session.question.respond` | `{sessionId, branchId, requestId, callId, answers? \| declined: true}` | `{}` | The call's own admission | `invalid`, `not_pending` |
| `session.question.pending` | `{sessionId, branchId, through?, after?}` | `{branchId, through, batches, next}` | The caller's token stops the read; the wire code is `operation_interrupted` | `invalid`, `cancelled` |

The tool's own outcomes are `ToolResult`s: answered, declined, non-interactive, invalid input, cancelled, and the part's reload settlement.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The answer names no question, repeats one, chooses an unoffered label, or the call's input is malformed; the wire code is `invalid_params` |
| `not_pending` | No waiting call matches the three ids; the wire code is `question_not_pending`; re-read `pending` |

The pending read's `invalid` is the wire's `invalid_params`; its `cancelled` is the wire's `operation_interrupted` and leaves the inventory unchanged.

### Flows

| Path | Normal | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Claim | Insert the entry before the append, keyed by `(line, request, call)` | The claim is released before dispatch | The entry is removed with the row | The part's unload removes every entry | Questions; the release is `release_claims` |
| Wait | The tool waits on the entry's sender | The call's token removes the entry; the result is `cancelled` | The claim's release removes the entry | The part's unload drops the senders; ToolExecutor settles the call | Questions |
| Answer | `respond` validates, removes the entry, sends the answer | The answer is delivered and then dropped with the claim; the call's result is its refusal | Nothing to release | The part's unload removes pending entries | Questions |

## Exceptions with reasons

- An answer accepted between the claim and a refused append is delivered and then dropped with the claim: the call's result is the refusal, and the question does not stay open.
- A wait left by a reload or restart is settled by ToolExecutor, not rebuilt: the waiter is a human I/O, and the line's facts do not hold it.
- `pending` reads the line's facts at the cutoff through `timelineTree.query`, not the waiter table: the batches are the line's published facts, and a waiting question is only the table's live entry.

## Verification

- Input rules: 1 to 4 questions, unique ids, 2 to 6 options, headers, custom text; a malformed input answers `invalid` before any entry is created.
- A claim inserts before the append; a refused append, an interrupt before dispatch, a refused admission, a cancelled task, a Loop error and a reload leave no entry.
- `respond` removes the entry and the waiting call answers; a second `respond` answers `not_pending`; an answer for another line's entry is `not_pending`.
- The part's unload removes every pending entry and the calls settle as reloading.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact. |
| §2 scopes | Session: the table serves the Session's lines; a branch part registers a Session namespace only through this part's placement. |
| §3 orthogonality | No other owner's state is read; the `interactive` flag is the creation record. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact; the claim is the tool mechanism's, not a fact. |
| §4 rule 6 | The answer reaches the model as the tool result. |
| §4 rule 7 | Clients use the two methods; no caller reads the table. |
| §4 rule 8 | No infrastructure change; the claim is the tool mechanism's contract. |
| §7 values, ids and callbacks | Ids and answers are data; the claim is a callback; no waiter object or sender crosses. |
| §7 no proxy | The table answers its own lines' questions. |
| §7 one interface | No service name; the tool and the two methods are the surface. |
| §7 cross-language | The methods are data; the tool's execute is a callback. |
| §7 one change | The implementation task migrates the claim to the tool contract's operations in one change. |
| ADR 002 D1 | Session scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The tool depends on the registry and the method table. |
| ADR 002 D4 | No setting. |
| ADR 002 D5 | The contract is data; the binding is below. |

## Requirements raised to mechanism owners

None beyond the claim lifecycle recorded in [ToolRegistry](ToolRegistry.md#requirements-raised-to-mechanism-owners). The pending read uses the Session's `timelineTree` service, already a dependency; the line id is a value from the execution context, so no branch access is needed.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `ask_user` tool's registration and the `session.question.respond`/`pending` handlers; the claim moves to `toolRegistry.claim`/`release_claims` and the methods stay the client's.

## Binding appendix

The claim callback is the tool contract's claim parameter (`register(..., claim)`); the method handler is the table's `call` callback. The waiter table and its entries stay private. The pending reply's batch type is `dim_agent_types::PendingQuestionBatch`.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondQuestion {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: String,
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<Answer>>,
    #[serde(default)]
    pub declined: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer { pub question_id: String, pub selected: Vec<String>, #[serde(default)] pub custom: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub session_id: String,
    pub branch_id: String,
    #[serde(default)] pub through: Option<u64>,
    #[serde(default)] pub after: Option<dim_agent_runtime::QueryCursor>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReply {
    pub branch_id: String,
    pub through: u64,
    pub batches: Vec<dim_agent_types::PendingQuestionBatch>,
    #[serde(default)] pub next: Option<dim_agent_runtime::QueryCursor>,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum QuestionError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("no ask_user call is waiting for this answer")]
    NotPending,
}
```

## Client attach and forks

### 3.2 Clients, attach and several clients

A client attaches first, then reads `session.question.pending` separately from the transcript window. It holds live events while the fixed-cut read is in flight, installs its question batches, then consumes held events above that cutoff. A batch result that arrives during loading closes the corresponding questions; old transcript events cannot reopen them. No Questions-specific notification is added to Rpc. A failed read leaves the inventory unavailable and retryable, not empty.

```text
history + live session.event, one branch
  seq 41 step/tool-call   id e41 {request_id: r1, calls: [ask_user c1 {questions}]}  -> show prompt for (r1, c1)
  seq 42 step/tool-result {call_event_id: e41, request_id: r1, results: [c1]}      -> close prompt for (r1, c1)
```

A client keeps the call's `requestId` with its prompt and sends it back with the answer. Several clients may answer; the first `respond` decides and the others get `question_not_pending`. A client that gets `question_not_pending` keeps its prompt until the batch's `step/tool-result` arrives, which closes it on every client.

### 4.3 Fork and branches

A Session fork cuts `main` only at a rest point with no tool proposal lacking its result ([session deletion and fork §6.2](../appserver/session-deletion-and-fork.md)), so a fork never holds a pending question. A child branch inherits only settled calls; a call cut without its result projects as not executed, and no entry exists for the child. Each branch's questions are answered with that branch's `branchId`.

Call ids come from model output and are unique, by convention, only within one batch: a provider may use `c1` again in a later request of the same branch, or in another branch. The key therefore also holds the `requestId` of the call's batch. The Loop gives every model request a new id and proposes at most one `step/tool-call` per request, so `(branchId, requestId, callId)` names exactly one call. A respond reaches only the call whose prompt the client showed:

```text
branch main, two clients
  T1 step/tool-call {request_id: r1, calls: [ask_user c1 "Deploy staging?"]}
       client A:       respond {requestId: r1, callId: c1, Yes}       -> {}
  R1 step/tool-result {request_id: r1, results: [c1]}                    turn 1 ends
  T2 step/tool-call {request_id: r2, calls: [ask_user c1 "Delete production data?"]}
       client B, late: respond {requestId: r1, callId: c1, Yes}       -> question_not_pending
       client B, late: respond {requestId: r1, callId: c1, declined}  -> question_not_pending
       client A:       respond {requestId: r2, callId: c1, No}        -> {}  (r2 still waited)
  R2 step/tool-result {request_id: r2, results: [c1 "... Delete production data? -> No"]}
```

| Scenario | Outcome |
| --- | --- |
| Client answers before the handler registered | `question_not_pending`; the batch has no result yet, so the client keeps the prompt and lets the user resend |
| Two clients answer | First accepted; second `question_not_pending`; both prompts close at `step/tool-result` |
| A late answer or decline to a settled call whose call id a later call reuses | `question_not_pending`: the `requestId` differs. The later call keeps waiting and accepts its own answer |
| `exec` without terminal input | The CLI stops the turn with exit code 3 |
| Steer, Queue, Append while waiting | No effect on the question; consumed after the batch or turn as usual |

## Previous sections

## 2. Data and state

See [Use and scope](#use-and-scope-responsibility-split) and [Own contract](#own-contract).

### 2.1 The claim before the fact

See [Own contract](#own-contract).

### 3.5 Pending-question read

See [Own contract](#own-contract).

## 9. Requirements raised to mechanism owners

See [Requirements raised to mechanism owners](#requirements-raised-to-mechanism-owners).

### A.1 Tool `ask_user`

See [Caller example](#caller-example) and [Binding appendix](#binding-appendix).
