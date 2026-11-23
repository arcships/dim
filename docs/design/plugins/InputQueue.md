# InputQueue

Scope: branch. Issue: #2027.

InputQueue owns when a line's inputs become applicable: it consumes committed inputs in order, decides when each is applied, withdrawn or deleted, and answers whether one is still pending. The current implementation is described in the [InputQueue plugin page](../../architecture/rust-agent/plugins/InputQueue.md).

## Use and scope: responsibility split

InputQueue owns:

- **The queue.** The inputs a line holds unapplied, their modes, the open turn and the starting flag, in commit order.
- **Application.** Steer and append inputs apply at once; a queue input applies when no turn runs or is about to start; a handover input waits for the turn it names and is deleted at that turn's end. The decisions produce `input/applied`, `input/deleted` and one replacement `input/message` for an accepted Steer, in one atomic batch.
- **The withdrawal decision.** Whether a held input is withdrawn, and the conditional replacement of an accepted Steer.
- **The pending read.** Whether an input is still held and unapplied at a published cutoff, including the block-presence answer the read contract defines.
- **The submission admission.** An input is accepted once per submission id; a repeated submission answers the committed input.

Callers keep as business:

- What an input says and which mode it uses: the operator entry, a client, a plugin.
- The operator's content validation and the submit checks it runs: the Session entries and SubmitChecks.
- The turn's run: Loop.
- What a client shows for the queue: the client reads the pending and history reads.

| Requirement (caller) | Can callers compose it from the operations below without breaking InputQueue's invariants? | Handling |
| --- | --- | --- |
| The operator entry writes a withdrawal, optionally replacing the input with a checked Steer | No: the conditional decision and the atomic replacement are the queue's; two calls would let a second withdrawal replace the deleted input | `withdraw` |
| A plugin or another owner writes an input to the line by line id | No: the submission admission, the order and the turn decision are the owner's | `submit` |
| A Session method answers whether an input is still pending | No: membership at a cutoff and the block-presence answer are the queue's folded state | `pending` |
| A client learns how many inputs wait | Yes: `pending` for the ids it knows, and the history read for their modes | Business |
| A client withdraws an input it wrote | Yes: `withdraw` without a Steer candidate | Business |

An operation exists only where the row says No.

## Caller example

The operator entry runs the content checks, then asks for the conditional withdrawal and replacement:

```rust
let answer = input_queue
    .withdraw(
        WithdrawRequest {
            input_id,
            reason,
            steer: Some(checked_candidate),
        },
        cancel.clone(),
    )
    .await?;
// Deleted { sequence } | NotHeld
```

A plugin submits an input into a line it does not own:

```rust
let InputRef { input_id, sequence } = input_queue
    .submit(SubmitRequest { input }, cancel.clone())
    .await?;
```

A Session method reads whether an input is still pending at a cutoff:

```rust
let Pending { pending } = input_queue
    .pending(PendingRequest { input_id, through: Some(head), include_block_presence: false }, cancel)
    .await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| SubmitChecks (#2026, Session) | `check` | The registered checks the operator's Steer must pass before the queue admits it. |
| BranchScope | `timeline`, `dispatcher`, `timelineIdentity`, `branchLifecycle` | The line's facts, their order, its identity and its phase. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `submit` | Admit one input into the line, once per submission id | `{input}` | `{inputId, sequence}`; a repeated submission id answers the committed input | The caller cancels before the append; nothing is written | `invalid` (the input fails its own shape or scope), `closed` |
| `withdraw` | Withdraw one held input, optionally replacing it with a checked Steer | `{inputId, reason?, steer?}` | `{status}`: `deleted{sequence}` or `notHeld` | The caller cancels the wait; a committed withdrawal stands and a later call answers `notHeld` | `input_blocked` (a submit check refused the Steer), `invalid` (a malformed Steer candidate), `publication_unknown` (committed but publication unconfirmed), `cancelled`, `closed` |
| `pending` | Whether an input is held and unapplied at a cutoff | `{inputId, through?, includeBlockPresence?}` | `{pending}` | The caller cancels the wait for the cutoff; nothing changes | `closed`, `cancelled` |

`submit` accepts an input whose submission id is new; the same id answers the input already committed, so a retry is safe. A Steer candidate in `withdraw` runs the registered checks in name order; the first block answers `input_blocked` and writes nothing, and the accepted candidate's blocks are appended to the operator's content before the replacement is written.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `invalid` | `submit`, `withdraw` | Fix the input: a malformed candidate, a scope the line does not accept |
| `input_blocked` | `withdraw` | The check's message says why; the withdrawal and the replacement were not written; change the input |
| `publication_unknown` | `withdraw` | The batch committed but its publication was not confirmed; query the line's facts after recovery instead of retrying the withdrawal |
| `cancelled` | `withdraw`, `pending` | The wait stopped; a committed withdrawal stands |
| `closed` | all | The line is closing; stop writing inputs to it |

A withdrawal of an input the queue no longer holds is `notHeld`, not an error: the caller's next step is the same, and a `notHeld` decision writes no result and no replacement.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `submit` | Admit the submission id, append the input through the line's entry, answer the committed id and sequence | Before the append: nothing is written | New submissions are refused; a committed input stays for the next generation | InputQueue: the append and its admission |
| `withdraw` | Admit the command, run the Steer's checks if any, decide against the held input, write the withdrawal command and the atomic result, answer after publication | The wait stops; a committed withdrawal and its replacement stand | An owed result is settled by the generation's recovery; a later generation answers from the facts | InputQueue: the decision and the result batch; SubmitChecks: a check's own work |
| `pending` | Read the current members at the cutoff, after the queue dispatched to it; answer the membership and the block presence | The wait stops | Pending reads end `closed`; the next generation rebuilds membership from the facts | InputQueue |

## Exceptions with reasons

- `withdraw` runs the operator's submit checks itself: the checks are registered on the Session, but only the queue knows whether the input is still held and only the queue can make the replacement atomic, so a Session entry composing the two would race a second withdrawal.
- A Steer candidate carries the operator's content and the checks' blocks as data: the candidate is never an adopted input and never enters model context, so it crosses with the command that decides it.

## Verification

- `submit` accepts one input per submission id; a repeated id answers the committed input; a new id appends in order.
- `withdraw` of a held input writes `input/deleted`; a `notHeld` decision writes no result and no replacement; a Steer candidate whose check blocks writes nothing and answers `input_blocked`.
- An accepted Steer's replacement `input/message` precedes the original's `input/deleted` in one batch, and a retry after a lost reply cannot replace the deleted input again.
- `pending` answers true only for an input held and unapplied at the cutoff; an input the queue does not hold is false; the block-presence answer follows the read contract.
- A reload rebuilds membership from the facts without reading input bodies; an owed accepted withdrawal is settled from its command.
- A call in flight when the line closes ends `closed`; a committed withdrawal keeps its result.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds input facts and writes application, deletion and replacement facts. |
| §2 scopes | Branch: the queue belongs to one line. A Session part writes an input by line id through `submit`. |
| §3 orthogonality | No other owner's state is read; the checks arrive through SubmitChecks' operation. |
| §4 rule 1 | A withdrawal is the `input/withdraw` command; the queue decides and writes the result. |
| §4 rule 2 | An applied input is handed to Loop by `input/applied`; after it only Loop's commands apply. |
| §4 rule 3 | Handlers register tasks and return; the submission checks run in the caller's call, not in a handler. |
| §4 rule 4 | No business meaning: the checks' meaning is their registrars'. |
| §4 rule 5 | The replacement and the deletion commit in one batch, replacement first. |
| §4 rule 6 | A Steer candidate is not an adopted input and never enters model context. |
| §4 rule 7 | Callers use the three operations; the checks are SubmitChecks'. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Applied and deleted results are published after their batch commits. |
| §7 values, ids and objects | Inputs, ids and the candidate are data; the result is a value; no object crosses. |
| §7 one interface | One name, `inputQueue`, on the `InputQueue` type; `withdraw_checked` and the `pending` Rust method are removed. |
| §7 cross-language | Every operation is data. |
| ADR 002 D1 | Branch: the queue is per line. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2026 (SubmitChecks): `check` must accept `{entry, branchId, user, mode}` and answer proceed with blocks and warnings or a block, with the caller's cancellation; InputQueue calls it for an operator Steer.
- #2026 (InputMethods): `session.input.pending` and `session.input.withdraw` become callers of `pending` and `withdraw` by line id; the Session entry keeps the operator's content validation and the client's reply shape.
- #2025 (Session access): a Session part writes an input to a line and reads its pending state by line id through the Session access surface.

## Binding appendix

The service is marked on the `InputQueue` type; the wire name stays `inputQueue`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::InputMessageData;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitRequest {
    pub input: InputMessageData,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRef {
    pub input_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithdrawRequest {
    pub input_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer: Option<InputMessageData>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Withdrawal {
    Deleted { sequence: u64 },
    NotHeld,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingRequest {
    pub input_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<u64>,
    #[serde(default)]
    pub include_block_presence: bool,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub pending: bool,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InputQueueError {
    #[error("invalid: {message}")]
    Invalid { message: String },
    #[error("input blocked: {message}")]
    InputBlocked { message: String },
    #[error("publication unknown")]
    PublicationUnknown,
    #[error("cancelled")]
    Cancelled,
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl InputQueue {
    pub async fn submit(&self, request: SubmitRequest, cancelled: CancelledFn) -> Result<InputRef, InputQueueError> { /* .. */ }
    pub async fn withdraw(&self, request: WithdrawRequest, cancelled: CancelledFn) -> Result<Withdrawal, InputQueueError> { /* .. */ }
    pub async fn pending(&self, request: PendingRequest, cancelled: CancelledFn) -> Result<Pending, InputQueueError> { /* .. */ }
}
```
