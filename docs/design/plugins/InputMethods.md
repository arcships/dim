# InputMethods

Scope: session. Issue: #2026.

InputMethods owns the Session's submit-check table and the `session.input` method contributions: the checks parts register before operator input is written, and the two methods that withdraw or read a line's pending input. The current implementation is in `crates/dim-agent-capabilities/src/input_queue.rs` and is described in the [InputQueue plugin page](../../architecture/rust-agent/plugins/InputQueue.md). The line's input queue is InputQueue's (#2027); this document states what the methods call.

## Use and scope: responsibility split

InputMethods owns:

- **Check registrations.** One check per registering row, with its callback and its lifetime; a registration ends when its row unloads.
- **The checked input.** Running every check in name order, combining their blocks and warnings, and placing the blocks after the operator's own content.
- **The two `session.input` methods.** Their declarations and handler; the input state stays with the line's InputQueue.

Callers keep:

- What a check means: Hooks and Skills decide; a check that fails is no opinion.
- When input is written: the operator-entry paths call `check` and write the returned input; the line's queue decides order, idempotency and withdrawal.
- The line an input belongs to: the caller names the line id.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part checks operator input for its row's lifetime | No: the duplicate check and the row's lifetime are the owner's | `register` |
| An entry turns a candidate input into the input it writes | No: the check order, the block placement and the drop rule are the owner's; a caller composing them forgets the placement | `check` |
| An entry withdraws a line's pending input | No: the conditional decision and the atomic replacement are InputQueue's, reached by line id | `session.input.withdraw` (the method below) |
| A client reads a line's pending inputs at a cutoff | No: the cutoff, the candidate selection and the applied-state lookup are the read's | `session.input.pending` (the method below) |
| A check refuses the input with its own code | Yes: `check` answers `blocked` with the check's message | Business |
| A check contributes context | Yes: `check` appends the check's marker blocks after the operator's content | Business |

An operation or parameter exists only where the row says No.

## Caller example

Hooks registers its `UserPromptSubmit` check from its `apply`:

```rust
checks.register(ctx, "hooks", |request| Box::pin(run_hooks(request))).await?;
```

An operator entry checks the input before writing it:

```rust
let checked = checks.check(
    SubmitEntry::Submit,
    line.clone(),
    data.user.clone(),
    data.effective_mode(),
    cancelled.clone(),
).await?;
let warnings = match checked {
    Checked::Proceed { user, warnings } => {
        session.input.commit(user, &line).await?;
        warnings
    }
    Checked::Blocked { message } => return Err(RpcError::app(-32000, "input_blocked", message)),
};
```

`session.input.withdraw` composes a command and the line's queue:

```rust
let receipt = line_owner::<InputQueue>(line.clone())?
    .withdraw(WithdrawInput { input_id, reason, steer }, cancelled.clone())
    .await?;
```

## Own contract

### Dependencies

None beyond the line's input owner, reached by line id.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a check for the calling row's lifetime | `{name, check}` | `{}` | None | `duplicate`, `invalid`, `closed` |
| `check` | Turn a candidate input into the input an entry writes | `{entry, branchId, user, mode}` | `{proceed: {user, warnings}}` or `{blocked: {message}}` | The caller's token stops the checks; a check already running stops with the token | `cancelled`, `closed` |
| `session.input.withdraw` | Withdraw a pending input on a line | `{sessionId, branchId, inputId, reason?, steer?}` | the queue's receipt | The caller's token; a committed withdrawal is not undone | `invalid`, `input_blocked`, `publication_unknown`, `cancelled`, `closed` |
| `session.input.pending` | A line's pending inputs at a cutoff, one page | `{sessionId, branchId, through?, after?}` | `{branchId, through, inputs, next}` | The caller's token stops the read | `invalid`, `cancelled`, `closed` |

`check` is a callback by reference; it never runs inside the Timeline's commit lock; its future is dropped when the caller's token fires. `check` returns the input the entry writes, with the checks' marker blocks already placed after the operator's content, and the warnings the client shows; a non-marker block is dropped with a diagnostic, as today.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate` | The check name is registered already; the row must not register twice |
| `invalid` | The check name is empty, the entry/mode pair is unknown, or the request is malformed |
| `cancelled` | The entry's call was interrupted while a check ran; the wire code is `operation_interrupted`; write nothing |
| `input_blocked` | A check refused the input; the message is the caller's error |
| `publication_unknown` | The withdrawal committed but its publication was not confirmed; read the line's state after recovery instead of retrying blindly |
| `closed` | This Session or the line is closing; write nothing |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the name and record the callback under the row | — | The registration is withdrawn | New registrations are refused | The registering row |
| `check` | Take the registered checks in name order, run each with the token, combine, place the blocks, answer | A running check stops with the token; the entry writes nothing | The checks' callbacks are revoked; the entry writes nothing | Pending checks end `closed` | InputMethods for the combination; each check for its own work |
| `session.input.withdraw` | Validate the input, run the checks, write the command to the line's queue, wait for publication, answer the receipt | A committed withdrawal is not undone; the wait ends `cancelled` / `operation_interrupted` | The command settles with the line | The line's queue answers by its own state | The line's InputQueue |
| `session.input.pending` | Read the line's facts at the cutoff, select the candidates, read their applied state, page | The read stops | Nothing | Answers the state it observed | TimelineTree for the read; InputMethods for the projection |

## Exceptions with reasons

- A check that fails counts as proceed: an input check is an opinion, and a failing check must not block the operator's input.
- The first block in name order is the answer; later checks still run for their warnings: the caller reads one refusal and every warning, as the operator-entry flow already does.
- `check` applies the marker rule itself: a check's block must be a runtime marker element, and an entry that forgot the rule would let a check's text read as the operator's words.

## Verification

- A registration lives until its row unloads; a duplicate name is refused; a check's failure is no opinion.
- `check` runs checks in name order; the first block ends the call and the checks after it are not run; every warning collected before it is discarded. A non-marker block is dropped; an input without operator content takes no blocks.
- A cancelled preparation runs no further check and writes nothing; a running check stops with the token.
- `session.input.withdraw` with a steer runs the checks before writing the command; a block answers `input_blocked` and writes nothing; the committed command's receipt carries the warnings.
- The two methods answer `invalid` for a malformed request and `closed` after the line ends.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact itself (the line's queue writes the withdrawal). |
| §2 scopes | Session: the check table serves the Session's entries; the input state is the line's. |
| §3 orthogonality | No other owner's state is read; the line's queue is reached by id. |
| §4 rules 1–5, 9 | No command event of its own, no handoff, no before callback, no batch, no Timeline fact. |
| §4 rule 6 | A check's blocks enter context only through the input the entry writes; this owner adds no path. |
| §4 rule 7 | Callers use these operations; no caller reads the check table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The candidate input and the receipt are data; the check is a callback; the line is an id; no object crosses. |
| §7 no proxy | The check table answers its own checks; the line's queue answers the input. |
| §7 one interface | One name, `submitChecks`, on the owner type; `append_blocks` leaves the public surface. |
| §7 cross-language | Every operation is data or a callback; no method exists only for Rust. |
| §7 one change | The implementation task deletes `append_blocks` and the separate check call, adds `check`, and migrates both entries in one change. |
| ADR 002 D1 | Session scope for the checks; the input state is the line's. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | A check follows its row's lifetime. |
| ADR 002 D4 | No setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. [#2027](https://github.com/arcships/dim/issues/2027): the line's InputQueue needs `withdraw` reached by line id, including the publication wait. The line mechanism is [#2127](https://github.com/arcships/dim/issues/2127) ([#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025)); until it exists the method cannot be migrated off `TreeCommands`. `withdraw` uses the `user` returned by `check` instead of appending blocks itself. The `session.input.pending` page reads the line through `timelineTree.query`; it needs no InputQueue operation.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `session.input.withdraw` and `session.input.pending` shapes; the client shapes stay, and the method table registration is `sessionMethods.register`.
- [Hooks A.6.2](Hooks.md#a62-submit-check-at-the-operator-input-entries): the check's behavior (name order, first block ends, warnings, marker placement) is unchanged; the registration is the direct callback below instead of `Arc<dyn CheckSubmission>`.
- [pending input read](../input-state-read.md): the read's data source moves from `TreeCommands` to `timelineTree.query`; the request, page and reply shapes are unchanged.

## Binding appendix

The service is marked on `SubmitChecks` under `submitChecks`. `append_blocks` stops being public. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is not a request field. A check's failure is no opinion, so the callback returns no error; the table treats a thrown error as proceed.

```rust
use crate::host::CancelledFn;
use crate::input_queue::{SubmitCheckReply, SubmitCheckRequest, SubmitEntry};
use dim_agent_types::{InputMode, SubmitWarning, UserInput};
use rutis::host::CallError;
use rutis::host::BoxFuture;
use rutis::Ctx;

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum InputError {
    #[error("{message}")]
    Duplicate { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("{message}")]
    Blocked { message: String },
    #[error("the withdrawal committed and its publication is unconfirmed")]
    Interrupted,
    #[error("the Session or line is closing")]
    Closed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
/// A check's own failure; the table treats it as no opinion and proceeds.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct CheckCallbackError { pub code: String, pub message: String }

impl From<CallError> for CheckCallbackError {
    fn from(error: CallError) -> Self {
        match error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>()) {
            Some(rutis_bridge::session::Error::Remote { name, message, .. }) => Self { code: name.clone(), message: message.clone() },
            Some(other) => Self { code: other.code().unwrap_or("failed").into(), message: other.to_string() },
            None => Self { code: "failed".into(), message: error.message().into() },
        }
    }
}

pub enum Checked {
    Proceed { user: Option<UserInput>, warnings: Vec<SubmitWarning> },
    Blocked { message: String },
}

/// The client's `session.input.pending` request and page, unchanged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub session_id: String,
    pub branch_id: String,
    #[serde(default)] pub through: Option<u64>,
    #[serde(default)] pub after: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReply {
    pub branch_id: String,
    pub through: u64,
    pub inputs: Vec<PendingInput>,
    #[serde(default)] pub next: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInput {
    pub input_id: String,
    pub sequence: u64,
    pub mode: InputMode,
    #[serde(default)] pub user: Option<UserInput>,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub has_raw_blocks: Option<bool>,
}

#[rutis::service(name = "submitChecks")]
impl SubmitChecks {
    pub async fn register(
        &self,
        caller: &Ctx,
        name: String,
        check: impl Fn(SubmitCheckRequest) -> BoxFuture<'static, Result<SubmitCheckReply, CheckCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), InputError> { /* ... */ }

    pub async fn check(
        &self,
        entry: SubmitEntry,
        branch_id: String,
        user: Option<UserInput>,
        mode: InputMode,
        cancelled: CancelledFn,
    ) -> Result<Checked, InputError> { /* ... */ }
}
```

`session.input.withdraw` and `session.input.pending` are registered with `sessionMethods.register`; their handlers take the client's data and call the line's InputQueue or `timelineTree.query`.
