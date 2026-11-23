# Wait

Scope: session (the tool) and branch (the arrival owner). Issue: #2026.

Wait owns the model's wait: the `wait` tool and the branch's arrival table. The current implementation is described in the [Wait plugin page](../../architecture/rust-agent/plugins/Wait.md). The tool is a Session part; the arrival table is a branch Capability (#2027's scope) reached by line id.

## Use and scope: responsibility split

Wait owns:

- **The arrival table.** Per line, the inputs that can end a wait (a background task's notice, a steer, a line's own events) from their release until the model reads them, and the single-wait slot.
- **One wait.** From the call to the first matching arrival, the timeout or the cancellation.
- **The tool declaration.** Its schema and result kinds.

Callers keep:

- When to wait: the model decides.
- What a task id means: BackgroundExec and Collaboration own their tasks.
- The notice's text: the producers write it; the arrival table records only that it arrived.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model waits on its line for a matching arrival | No: the arrival table, the single slot and the reload rule are the owner's | `wait` |
| The model asks for one task or any arrival | Yes: the same operation with an optional task id | Business |
| A second wait runs beside the first | No: the single-wait slot is the owner's | `wait` answers `busy` |
| Read what already arrived | Yes: `wait` with a zero or small timeout answers the arrival at once, or the notice in the model's context | Business |

An operation or parameter exists only where the row says No.

## Caller example

The tool runs on the calling line and waits there:

```rust
let line = execution.branch_id; // the line id the call carries
let WaitReply { ended } = line_owner::<Wait>(line)?.wait(WaitRequest {
    task_id,
    timeout_ms: 300_000,
}, cancelled.clone()).await?;
match ended {
    Ended::Notification { task_ids, waited_ms } => result_text(task_ids, waited_ms),
    Ended::Steer { waited_ms } => result_text_steer(waited_ms),
    Ended::AlreadyNotified { task_ids, waited_ms } => already(task_ids, waited_ms),
    Ended::Timeout { waited_ms } => timed_out(waited_ms),
    Ended::Interrupted { waited_ms } => interrupted(waited_ms),
}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:dispatcher` | Consume `input/message`, `turn/start` and `turn/end` in order and record arrivals. |
| `B:timeline` | Read the line's own facts at the branch's cut; commit no fact from here. |
| `B:timelineIdentity` | Name the line this table belongs to. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `wait` | Wait on this line for a matching arrival, the timeout or the cancellation | `{taskId?, timeoutMs?}` | `{ended}`: `notification {taskIds, waitedMs}`, `steer {waitedMs}`, `alreadyNotified {taskIds, waitedMs}`, `timeout {waitedMs}`, `interrupted {waitedMs}` | The caller's token ends the wait as `interrupted` and releases the slot; a dropped Rust future releases the slot | `busy`, `cancelled`, `closed` |

`timeoutMs` is 1 through 300,000, default 300,000. The slot is single: a second wait on the same line answers `busy` until the first ends or its caller drops it.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `busy` | A wait is running on this line; end it or retry after it ends |
| `cancelled` | The call stopped before any arrival; nothing was consumed |
| `closed` | The line is ending or the table is unloading; wait on the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `wait` | Take the slot, return an already-arrived match, otherwise wait for a release, the timeout or the token | The token ends the wait `interrupted` and releases the slot; the arrival stays recorded | The dropped call releases the slot; the arrival stays for a later wait or read | The generation's unload ends an accepted wait `closed`; the arrivals are rebuilt from the line's facts | Wait for the slot and the result; the caller for its dropped future |

## Exceptions with reasons

- An arrival taken by a wait stays recorded: a later read of the same notice in the turn is told it was already delivered, so a wait never consumes the model's only copy.
- A branch close cancels a wait without a wake: the line is ending, and the model's next turn is on another line.
- The table folds the line's own facts at open: wait has no persistent state of its own, and the branch rebuilds the same arrivals after a reload.

## Verification

- A wait ends on the matching completion, not on the input that started the turn; a steer or a stop ends it; a second wait answers `busy`.
- A notice the model already read does not end a later wait; the wait answers `alreadyNotified`.
- Cancelling a wait ends it `interrupted` and releases the slot; dropping the future releases the slot.
- An arrival recorded before the call is answered at once; a reload rebuilds the arrivals from the line's facts.
- The line's end ends a wait with `closed` and no wake.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | The tool is a Session part, not a Capability; the arrival table is a branch Capability: it folds the line's facts in order and writes none. |
| §2 scopes | The tool is Session; the table is branch; the tool reaches the table by line id. |
| §3 orthogonality | No other owner's state is read; the task ids are values. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact from the table. |
| §4 rule 6 | The wait's result reaches the model as the tool result; notices reach it as inputs their producers write. |
| §4 rule 7 | The tool calls the table's operation; no caller reads the arrivals. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The request and result are data; task ids and the line id are values; the cancellation is an argument; no object crosses. |
| §7 no proxy | The table answers its own arrivals; the tasks belong to their owners. |
| §7 one interface | One name, `wait`, on the branch owner; the Session tool only registers. |
| §7 cross-language | The operation is data; no method exists only for Rust. |
| §7 one change | The implementation task migrates the tool off `TreeCommands` in one change. |
| ADR 002 D1 | Branch scope for the arrivals; Session for the tool. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The tool depends on the line's table by id, not on the table's implementation. |
| ADR 002 D4 | The timeout default is the owner's; no setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): the line access that lets the Session tool reach the line's `wait` by `(session, line)` id; the current call takes it from `TreeCommands.get(line)?.service::<Wait>()`.

## Binding appendix

The service is marked on `Wait` under `wait`. `caller: &Ctx` fills the line identity; `CancelledFn` is not a request field.

```rust
use crate::host::CancelledFn;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Ended {
    Notification { task_ids: Vec<String>, waited_ms: u64 },
    UserSteer { waited_ms: u64 },
    AlreadyNotified { task_ids: Vec<String>, waited_ms: u64 },
    Timeout { waited_ms: u64 },
    Interrupted { waited_ms: u64 },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitReply { pub ended: Ended }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitError {
    Busy { message: String },
    Cancelled,
    Closed,
}
```
