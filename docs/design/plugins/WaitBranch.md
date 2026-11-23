# WaitBranch

Scope: branch. Issue: #2027.

WaitBranch owns one line's wake-ups: the steers and background-task notices released on the line during an open turn that the model has not read yet, and the one wait a caller may park on them. The `wait` tool that calls this owner is a Session part of #2026's registry group; this document is the branch part's target contract. The division is: the Wait document (the tool's owner, #2026) owns the tool declaration, its model-facing texts and the tool-result settlement, and registers no arrival table and no `wait` operation; the arrival table, the single slot and the `wait` operation are this contract's.

## Use and scope: responsibility split

WaitBranch owns:

- **Arrivals.** The inputs released on the line during an open turn: a Steer, and a background task's tagged notice. An input the model has already read, or one deleted before it read, no longer counts.
- **The wait slot.** One wait per line; a second concurrent wait is refused.
- **The wait call's lifetime.** A caller-owned call bound to the owner's generation: the caller ends it with its cancellation, and the owner ends it at its close or reload; a reload does not answer `already_waiting`.
- **The timeout.** The caller's bound on this wait, not on the task it waits for.

Callers keep as business:

- Which task a caller waits for, and what to do after a wake-up: the tool and its model-facing texts.
- Which inputs are sent to the line: the operator entry and EventHub.
- The turn's run: Loop.

| Requirement (caller) | Can callers compose it from the operations below without breaking WaitBranch's invariants? | Handling |
| --- | --- | --- |
| The `wait` tool parks until a matching arrival, a Steer, the timeout or its own cancellation | No: the arrival set, the single slot and the generation's settlement are the owner's, and a caller polling the Timeline would read state it does not own | `wait` |
| A caller wants the tasks that finished | Yes: the reply names the task ids | Business |
| A client reads whether the model already read a notice | Yes: the history read answers the consumed input | Business |

An operation exists only where the row says No.

## Caller example

The `wait` tool, on the calling line, passes the line id, the optional task and its own cancellation:

```rust
let outcome = line_owner::<WaitBranch>(line)?
    .wait(
        WaitRequest { task_id, timeout_ms },
        cancel.clone(),
    )
    .await?;
match outcome {
    WaitOutcome::Notification { task_ids, .. } => text(&task_ids),
    WaitOutcome::UserSteer { .. } => "wait ended: steer".into(),
    WaitOutcome::AlreadyNotified { task_ids, .. } => already(&task_ids),
    WaitOutcome::Timeout { .. } => "wait ended: timeout".into(),
    WaitOutcome::Interrupted { .. } => "wait ended: interrupted".into(),
}
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| BranchScope | `dispatcher`, `timeline`, `branchLifecycle` | The line's facts in order, its committed head and its phase. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `wait` | Wait for the next matching arrival, a Steer, the timeout or the caller's cancellation | `{taskId?, timeoutMs}` | `{reason, taskIds?, waitedMs}`: `notification`, `user_steer`, `already_notified`, `timeout` or `interrupted` | The caller's cancel ends the wait as `interrupted` and releases the slot; a dropped native future releases it too | `already_waiting` (another wait holds the line's slot), `closed` |

`timeoutMs` is this wait's bound, from 1 to the tool's cap; the owner does not limit task execution. A wait that ends because the model already read the task's notice answers `already_notified` with the task ids instead of waiting for a notice that will not come again. A reload ends an accepted wait with `closed`; the tool maps that to the existing reload settlement, and the turn fails without a continuation.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `already_waiting` | `wait` | One wait runs on the line; end it or report that a wait is already running |
| `closed` | `wait` | The line is closing or reloading; the tool settles its call as a reload and the turn fails |

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `wait` | Admit under the slot, register the call in the generation's tracker, wait for an arrival, the timeout or cancellation, release the slot, answer | The cancellation ends the wait as `interrupted`; the slot is released before the tracking token | Admission stops; active calls end `closed` and the generation waits for their slots; a new generation starts with no old arrivals | WaitBranch: the slot and the arrival set; the tool: the result text and the tool-result fact |

## Exceptions with reasons

- A reload ends an accepted wait with an error rather than replaying an arrival: a cancelled background task emits no wake, and an arrival from the previous generation is not a wake the model can still act on.
- The wait's timeout belongs to the call, not to the owner: the owner has no policy on how long a caller may park, and the tool is the one place a model-facing bound is stated.

## Supersedes

- [Wait](https://github.com/arcships/dim/pull/2103) (#2103): the branch arrival table and its `wait` operation; the tool declaration, its texts and the tool-result settlement remain that document's.

## Verification

- A Steer or a tagged notice released during an open turn ends a wait; an input the model read first does not end a later wait; the input that opened the turn never ends one.
- `wait` with a task id wakes only on that task's notice; without one it wakes on any matching arrival.
- A second concurrent wait answers `already_waiting`; the first releases its slot on cancellation, on timeout and on drop.
- A reload ends an accepted wait with `closed`; the tool writes the marked error result and the turn fails without continuing.
- An arrival from before a reload is not replayed; the new generation rebuilds only whether a turn is open.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds the line's input and turn facts; it writes no fact of its own. |
| §2 scopes | Branch: arrivals and the slot belong to one line. A Session part calls `wait` by line id. |
| §3 orthogonality | No other owner's state is read; the arrivals are the line's facts. |
| §4 rule 1 | No command event: the wait answers its caller and writes nothing. |
| §4 rule 2 | No handover. |
| §4 rule 3 | The wait is the caller's call, not a handler's; the handler only rebuilds the open-turn state. |
| §4 rule 4 | No business meaning: the model-facing texts are the tool's. |
| §4 rule 5 | No batch. |
| §4 rule 6 | The wait adds no context; its reply is a tool result. |
| §4 rule 7 | Callers use `wait`; the owner folds public facts. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and objects | The request and the reply are data; no handle or channel crosses. |
| §7 one interface | One name, `wait`, on the `WaitBranch` type; the private `arrival` path used by the tool is removed. |
| §7 cross-language | The operation is data, including the optional task id and the timeout. |
| ADR 002 D1 | Branch: the arrivals belong to one line's turn. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2026 (`wait` tool): the tool calls `wait` by the calling line id and keeps the model-facing texts and the tool-result settlement; it no longer holds the branch `Wait` object or calls a private method. The Wait document keeps only the tool; the arrival table and the operation are this contract's, so the two documents do not both define a table or a reply shape.
- #2025 (Session access): the tool calls `line_owner::<WaitBranch>(line)?.wait(..)`; ADR 027's id-to-instance step resolves the branch group's data face, except registration points and lifecycle services.

## Binding appendix

The service is marked on the `WaitBranch` type; the wire name stays `wait`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum WaitOutcome {
    Notification { task_ids: Vec<String>, waited_ms: u64 },
    UserSteer { waited_ms: u64 },
    AlreadyNotified { task_ids: Vec<String>, waited_ms: u64 },
    Timeout { waited_ms: u64 },
    Interrupted { waited_ms: u64 },
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum WaitError {
    #[error("wait is already running in this session.")]
    AlreadyWaiting,
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl WaitBranch {
    pub async fn wait(&self, request: WaitRequest, cancelled: CancelledFn) -> Result<WaitOutcome, WaitError> { /* .. */ }
}
```
