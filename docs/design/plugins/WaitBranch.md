# WaitBranch

Scope: branch. Issue: #2027.

WaitBranch owns one line's wake-ups: the steers and background-task notices released on the line during an open turn that the model has not read yet, and the one wait a caller may park on them. The `wait` tool is the Wait owner's Session part (#2026) and registers with `toolRegistry`; this document is the branch part's target contract, and the current implementation is described in the [Wait plugin page](../../architecture/rust-agent/plugins/Wait.md). The division is: the Wait document (the tool's owner, #2026) owns the tool declaration, its model-facing texts and the tool-result settlement, and registers no arrival table and no `wait` or `state` operation; the arrival table, the single slot and the two operations are this contract's.

## Use and scope: responsibility split

WaitBranch owns:

- **Arrivals.** The inputs released on the line during an open turn: a Steer, and a background task's tagged notice. An input the model has already read, or one deleted before it read, no longer counts. The arrivals and the notices the model read are per turn and `turn/end` clears both. The read-notice record is separate from the arrival table: a tagged notice released as the turn's opening input never enters the arrivals, and reading it still records its task.
- **The open-turn admission.** A wait is admitted only while the line's turn is open, because the input that opens a turn is released before `turn/start`; a call while it is closed answers `NoTurn`.
- **The wait slot.** One wait per line; a second concurrent wait is refused.
- **The admitted wait's state.** One read of the line's admitted wait: whether one runs, when it was admitted, and the task it names.
- **The wait call's lifetime.** A caller-owned call bound to the owner's generation: the caller ends it with its cancellation, and the owner ends it at its close, a turn end or a reload; the next generation admits a wait without answering `AlreadyWaiting` for one that ended. The `wait` tool's call is ended by ToolExecutor's cancellation before Loop writes `turn/end`; a caller that outlives the turn, a Session part or a bridge caller, is ended by the owner at `turn/end`.
- **The timeout.** The caller's bound on this wait, not on the task it waits for.

Callers keep as business:

- Which task a caller waits for, and what to do after a wake-up: the tool and its model-facing texts.
- Which inputs are sent to the line and which mode and tag they carry: the operator entry and a client's Steer, the `branch` tool's messages between lines ([timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)), EventHub's deliveries ([publish.rs](../../../crates/dim-event-hub/src/publish.rs)), and a background task's notice ([background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs)).
- The turn's run: Loop.
- What a client shows for a wait it reads: its own text and layout, from `state` and the tool result ([#1659](https://github.com/arcships/dim/issues/1659)); the App Server reads `state` and answers the client through that client-protocol addition.

| Requirement (caller) | Can callers compose it from the operations below without breaking WaitBranch's invariants? | Handling |
| --- | --- | --- |
| The `wait` tool parks until a matching arrival, a Steer, the timeout or its own cancellation, and answers `AlreadyNotified` for a task whose notice the model already read | No: the arrival set, the read-notice record, the single slot and the generation's settlement are the owner's, and a caller polling the Timeline would read state it does not own | `wait` |
| A caller wants the task that finished | Yes: the reply names the task id and the time waited | Business |
| A client reads whether the model already read a notice | Yes: the history read answers the consumed input | Business |
| The CLI shows a named wait while the call runs ([#1659](https://github.com/arcships/dim/issues/1659)) | No: the admission time and the admitted task are the owner's, and a client watching a tool call would claim a wait the owner never admitted | `state` |

An operation exists only where the row says No. `state` is the positive admission signal: the App Server reads the line's admitted wait by `(sessionId, branchId)` ([ADR 027](../../adr/027-cross-scope-access-by-id.md) D1) and answers the CLI through the method or notice the Rpc owner defines ([#1659](https://github.com/arcships/dim/issues/1659)), a client change that needs a maintainer decision. A batch such as `[exec, wait]` claims no wait before that read answers one. At most one wait is admitted per line, so the reply needs no call id.

## Caller example

The `wait` tool, on the calling line, reaches this owner by line id and passes the optional task and its own cancellation.

```rust
let cancelled: CancelledFn = Arc::new(move || {
    let cancel = cancel.clone();
    Box::pin(async move {
        cancel.cancelled().await;
        Ok(())
    })
});
let result = line_owner::<Wait>(line)?
    .wait(WaitRequest { task_id, timeout_ms }, cancelled)
    .await;
let content = match result {
    Ok(WaitResult::Notification { task_ids, .. }) => {
        format!("wait ended: notification ({})", task_ids.join(", "))
    }
    Ok(WaitResult::UserSteer { .. }) => "wait ended: steer".into(),
    Ok(WaitResult::AlreadyNotified { task_ids, .. }) => already(&task_ids),
    Ok(WaitResult::Timeout { .. }) => "wait ended: timeout".into(),
    Ok(WaitResult::Interrupted { .. }) => "wait ended: interrupted".into(),
    Err(error) => return settle(error),
};
```

`settle` is the tool's own wording: a refusal text for `AlreadyWaiting` and `NoTurn`, and the reload settlement for `Reloading`.

The App Server reads the same operation for a client: it resolves the line by `(sessionId, branchId)` ([ADR 027](../../adr/027-cross-scope-access-by-id.md) D1) and calls `state()`, whose reply is `WaitState {waiting, startedAt?, taskId?}`.

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| BranchScope | `dispatcher`, `timeline` | The line's facts in order and its committed head. |

The `wait` tool is a caller reached by line id ([ADR 027](../../adr/027-cross-scope-access-by-id.md) D1), not a dependency: WaitBranch injects nothing of the Session's.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `wait` | Wait for the next matching arrival, a Steer, the timeout or the caller's cancellation; a wait naming a task also answers `AlreadyNotified` when the model already read that notice | `{taskId?, timeoutMs}` | `WaitResult`: `Notification {taskIds, waitedMs}`, `UserSteer {waitedMs}`, `AlreadyNotified {taskIds, waitedMs}`, `Timeout {waitedMs}` or `Interrupted {waitedMs}` | The caller's cancel ends the wait as `Interrupted` and releases the slot; a dropped native future releases it too | `AlreadyWaiting` (another wait holds the line's slot), `NoTurn` (no turn is open), `Unavailable` (the committed head cannot be read), `Reloading` (an accepted wait on a closing line or a reloading part; a new call on a closing line is answered by the route as line-gone) |
| `state` | Read the line's admitted wait: whether one runs, its admission time and the task it names | `{}` | `WaitState {waiting, startedAt?, taskId?}`; `startedAt` is the admission time in milliseconds since the Unix epoch, and both fields are present only when `waiting` is true | None: the read does not wait | `Unavailable` (the committed head cannot be read), `Reloading` (a call that lands on an ended generation; a closing line is answered by the by-id route as line-gone before the read reaches the owner) |

`timeoutMs` is this wait's bound, and the tool holds its model-facing range of 1 through 300,000 milliseconds; the owner uses the timeout the call carries and has no policy on how long a caller may park, so it bounds no task execution. A wait is admitted only while the turn is open, so a call while it is closed answers `NoTurn`; the open-turn state is the one the owner folded from dispatched facts, rebuilt from the committed head and its permitted publishing tail when a generation starts, and confirmed there before a call is decided, so a committed boundary that has not been dispatched yet is not missed. The confirmation is a Timeline read: a failed read answers `Unavailable` and changes nothing. The read only refreshes the folded value; the generation, the refreshed open-turn state and the slot are then checked under one lock, so a `turn/end` that dispatches after the read still ends an admitted call. `state` answers from the same confirmed value, so a committed `turn/end` that has not been dispatched yet makes it answer `waiting: false`. Only an input released during an open turn is an arrival, and the input that opened the turn is never one, because it is released before `turn/start`. A wait naming a task answers `AlreadyNotified` when the model read that task's notice in this turn and no eligible arrival is pending: a pending Steer, or a pending notice the caller accepts, is taken first, and the notice it read is not waited for again. The read-notice record covers every tagged notice the model read in this turn, including one released as the turn's opening input. `UserSteer` names any Steer input, from the operator, another line or EventHub, not only one the user typed. A turn end ends a wait that outlives the turn with `Interrupted` and releases its slot, and the arrivals and read notices from that turn are cleared; the `wait` tool's call is ended by ToolExecutor's cancellation before Loop writes `turn/end`, so the tool writes no result for a turn that already ended. A reload ends an accepted wait with `Reloading` instead of replaying an arrival: a cancelled background task emits no wake, and an arrival from the previous generation is not a wake the model can still act on. The tool settles that call as a reload, and the turn fails without a continuation. `state` reads the same slot and arrival set without taking the slot: it answers the admitted wait or that none runs, so a client never infers a wait from a tool call that the owner has not admitted.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `AlreadyWaiting` | `wait` | One wait runs on the line; report that a wait is already running |
| `NoTurn` | `wait` | No turn is open on the line; the tool reports the refusal and a call made in a turn is admitted |
| `Unavailable` | `wait`, `state` | The committed head cannot be read, so nothing was decided; the tool reports a tool error and the turn continues, and a `state` caller reports that it cannot confirm and reads again |
| `Reloading` | `wait`, `state` | The tool settles its call as a reload and the turn fails; a `state` caller reports that no wait runs and reads again after the reload; a `wait` or `state` call on a closing line gets the route's line-gone and stops |

The `AlreadyWaiting` message keeps the tool's existing text, which names the Session; the slot is one per line. The next steps above are the `wait` caller's, which is the tool; a Session part that calls `wait` reports the same refusal to its own caller and retries when the condition changes. The App Server's `state` read has only `Unavailable` and `Reloading` to report.

### Flows

| Operation | Normal path | No turn is open | Caller cancels or unloads | Turn ends | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- | --- |
| `wait` | Confirm the open turn at the committed head, answering `Unavailable` if the read fails; admit under the slot, register the call in the generation's tracker, wait for an arrival, the timeout or cancellation, release the slot, answer | Answer `NoTurn` and hold no slot | The cancellation ends the wait as `Interrupted`; the slot is released before the tracking token; a bridge caller must cancel, and dropping its reply alone does not release the slot | The owner ends a wait that outlives the turn with `Interrupted` and releases its slot; the arrivals and the notices read in that turn are cleared. The `wait` tool's call is ended by ToolExecutor's cancellation before Loop writes `turn/end`, so its result belongs to that turn | Admission stops; an accepted wait on a closing line ends `Reloading`, and a reload ends the generation the same way and waits for active slots; a new call on a closing line is answered by the route as line-gone; a new generation starts with no old arrivals | WaitBranch: the slot, the arrivals, the `Interrupted` turn end and the `Reloading` generation end; the Wait tool (#2026): the result text; ToolExecutor: the tool-result fact |
| `state` | Confirm the open turn at the committed head, answering `Unavailable` if the read fails, then answer `waiting: false` for a confirmed closed turn, or the line's admitted wait with its start and task, or that none runs | Answer `waiting: false`, because no wait is admitted while the turn is closed | Nothing to cancel: the read does not wait and holds no slot | The admitted wait ends at the turn end, so a read afterwards answers none | A reload ends the generation, so a call that lands on it answers `Reloading` and the next generation answers none; a closing line ends the part and the route reports the line gone | WaitBranch |

Admission checks the generation, then the confirmed turn, then the slot: `Reloading` when the generation has ended, `Unavailable` when the committed head cannot be read, `NoTurn` when the turn is closed, `AlreadyWaiting` when another call holds the slot.

## Client-visible changes

The `wait` data face takes one request object and omits `taskId` instead of passing `null`; no existing client-protocol message changes, the thrown error's `data` stays undefined, and the tool answers the new `NoTurn` and `Unavailable` codes with texts of its own. The `state` read and its `WaitState` reply are additive, and the client path for it is the addition recorded below.

The named-wait display's client path is the App Server's: it reads `state` by `(sessionId, branchId)` and answers a protocol v4 client through a method or notice the Rpc owner defines for [#1659](https://github.com/arcships/dim/issues/1659). That client-protocol addition is a maintainer decision recorded in [cli-current-activity.md](../cli-current-activity.md), not a change of this face.

| Change | Reason | Affected | Migration |
| --- | --- | --- | --- |
| `wait(taskId, timeoutMs, cancelled)` becomes `wait(request, cancelled)`, where the request is `WaitRequest {taskId?, timeoutMs}` | the face carries its parameters as one request value, so the optional task and the timeout serialize together | a TypeScript caller of `wait` | pass the two fields as one object, omit `taskId` instead of passing `null`, and keep the cancellation callback |
| `state` is a new read | a client showing a named wait needs a positive admission signal and the authoritative start | a caller that shows a running wait | new call: `state()` answers `{waiting, startedAt?, taskId?}`; additive, so no existing caller changes |
| the error kinds add `NoTurn` and `Unavailable`; `AlreadyWaiting` and `Reloading` keep their codes and meanings | the opening input of a turn is released before `turn/start`, and confirming the committed head can fail | a caller matching a thrown code | report `NoTurn` instead of parking and call again during a turn; report `Unavailable` and retry, because nothing was decided |

## Exceptions with reasons

None: the owner hands out no object, and every call that is polled ends in a value or one of its errors.

## Supersedes

- `docs/design/service-faces.md` section 3.4: the `wait` row's operations, now `state()` and `wait(request, cancelled)`, and its link to the Wait plugin page's data face; the row names the operations and links this contract.
- `docs/design/cli-current-activity.md` section "Owner requirement for named waits": the open questions about a positive admission signal and an authoritative start are answered by `state` read by the App Server; the client path is the separate maintainer decision recorded there.
- `docs/design/examples/wait-contract-experiment.md`, `docs/design/examples/wait-argument.md`, `docs/design/examples/wait-usage-context.md` and `docs/design/research/remote-plugins.md`: the sentences and table cells that name the Wait plugin page as the contract, the owner's data face or the `wait` service's target now name this contract, and the cells that stated a dropped reply releases the slot or that a wait can be called while no turn is open now state this contract's rule.
- `docs/design/timeline-branches.md` and `docs/zh/design/timeline-branches.md`: the input-mode cells and the opener's waiting sentence that name `wait` now link this contract; `docs/design/examples/capability-models/coverage-audit.md` states this contract's `wait` call shape.
- `docs/design/capability-document-review.md`, `docs/design/examples/owner-handoff-verification.md`, `docs/design/examples/timeline-capability-analysis.md` and `docs/design/examples/timeline-contract-coverage.md`: the rows and lists that name the Wait plugin page as the owner design or recovery contract now name this contract; the page stays as the current implementation.
- `docs/architecture/rust-agent/plugins/Wait.md` § Data face: the face it prints is the current implementation, and the target it names is this contract.

No other design document defines this owner's surface. The Wait document (its owner is #2026) keeps the tool declaration, its texts and the tool-result settlement, and the [Wait plugin page](../../architecture/rust-agent/plugins/Wait.md) describes the current implementation.

## Verification

- A Steer or a tagged notice released during an open turn ends a wait; an input the model read first does not end a later wait; the input that opened the turn never ends one.
- `wait` with a task id wakes on that task's notice or on any Steer, so a Steer ends it while the named task is unfinished.
- Without a task id, `wait` wakes on any arrival: a Steer or any task's notice.
- A wait while no turn is open answers `NoTurn` and holds no slot; the line admits a wait once its turn opens.
- A second concurrent wait answers `AlreadyWaiting`; the first releases its slot on cancellation, on timeout and on drop.
- `turn/end` ends a caller that outlives the turn with `Interrupted` and releases its slot; the arrivals and the notices read in that turn are not carried into the next turn. The `wait` tool's call is ended by its cancellation before the turn ends and writes no result after it.
- A wait naming a task answers `AlreadyNotified` when the model read that task's notice in this turn and no eligible arrival is pending: a pending Steer answers `UserSteer` even then, and a notice the model read as the turn's opening input answers `AlreadyNotified`.
- A `wait` after a committed `turn/start` that is not yet dispatched is admitted, and one after a committed `turn/end` answers `NoTurn`; `state` answers `waiting: false` in that window.
- A failed committed-head read answers `Unavailable` and decides nothing: the tool reports a tool error and the turn continues, and a `state` caller retries on its next read.
- A `turn/end` that dispatches between the confirmation and the lock makes that call answer `NoTurn` because it is not admitted; a call admitted before the dispatch ends with `Interrupted`.
- A wait answers `Timeout` when its bound elapses, and `Interrupted` when the caller's cancellation ends it.
- `state` answers `waiting: true` with the admission time and, when the wait names one, the task id while a wait is admitted, and `waiting: false` otherwise; the read takes no slot and ends no wait.
- `state` answers `Reloading` for an ended generation; the next generation answers that no wait runs. A batch such as `[exec, wait]` claims no wait before `state` answers one.
- A reload ends an accepted wait with `Reloading`; the tool ends the call through the existing reload settlement, and the turn fails without continuing.
- A closing line ends an accepted wait with `Reloading`; a `wait` or `state` call on a closing line is answered by the by-id route as line-gone.
- An arrival from before a reload is not replayed; the new generation rebuilds only whether a turn is open.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds the line's input and turn facts; it writes no fact of its own. |
| §2 scopes | Branch: arrivals, the slot and the admitted wait belong to one line. A Session part calls `wait` by line id, and the App Server reads `state` by `(sessionId, branchId)`. |
| §3 orthogonality | No other owner's state is read; the arrivals are the line's facts. |
| §4 rule 1 | No command event: the wait answers its caller and writes nothing. |
| §4 rule 2 | No handover. |
| §4 rule 3 | No `before` callback: the handler folds the line's input and turn facts and returns, and the wait is the caller's call rather than a write hook. |
| §4 rule 4 | The Timeline, Dispatcher, `before` and batches gain nothing: the arrival kinds and the wait's meaning stay in this part, and no core module special-cases it. |
| §4 rule 5 | No batch. |
| §4 rule 6 | The wait adds no context; its reply is a tool result. |
| §4 rule 7 | Callers use `wait` and `state`; the owner folds public facts. |
| §4 rule 8 | No infrastructure change of its own; the `wait` tool's access by line id and the App Server's by `(sessionId, branchId)` are [ADR 027](../../adr/027-cross-scope-access-by-id.md) D1 and [#2127](https://github.com/arcships/dim/issues/2127). |
| §4 rule 9 | No Timeline fact. |
| §7.1 design questions | Resources: the arrivals are values folded from the line's facts, the slot and the admitted wait are the live call state, and no resource outlives a call. Lifecycle: a call ends with an arrival, `AlreadyNotified`, the timeout, the caller's cancellation or, for a caller that outlives the turn, the turn's end; the owner ends its generation's calls with `Reloading`, the Wait tool writes the result text and ToolExecutor the tool-result fact. Callers: two operations, `state` and `wait`, with four errors on `wait` and two on `state`; the CLI's running-wait display reads `state`. |
| §7.2 every call lands on one operation | The `wait` tool calls `wait`, the App Server reads `state` by `(sessionId, branchId)`, and a cross-language caller calls the same operations; no other entry exists. |
| §7.2 only values, views and callbacks cross | `state` is a read-only view, the request and the reply are data, and `cancelled` is the call's own callback. |
| §7.2 no relay, no peek | The owner returns no other owner's data and reads no other owner's state: the arrivals and the live open-turn state come from the dispatcher, and open-turn status is restored from the committed head and its permitted publishing tail, confirmed there before each admission. |
| §7.2 one face, no forwarding | Two operations on the `Wait` type, `state` and `wait`; the private `arrival` path the tool used is removed. |
| §7.2 data only on the public face | The request and the reply are data, the error is a code and a message, and no handle or channel crosses. |
| §7.2 no caller business concepts | The request carries a task id and a timeout; the model-facing texts, the task policy and the turn stay with the callers. |
| §7.2 no owner-internal mechanism pushed out | The arrival set, the slot, the generation tracker and the `Reloading` decision stay inside; the admission time leaves only as the value `state` publishes. The caller passes its cancellation or reads a value, and the tool owns the model-facing text while ToolExecutor writes the tool-result fact. |
| §7.2 cross-scope lifecycle | A Session part calls `wait` by line id and the App Server reads `state` by `(sessionId, branchId)` (ADR 027 D1). A call belongs to the generation that admitted it: the owner ends it `Reloading` when that generation ends and `Interrupted` at a turn end for a caller that outlives the turn, the tool and ToolExecutor settle the ended call, and a caller that abandons a native call releases the slot on drop. |
| §7.2 one change | The request object, the `state` read, every caller's migration and the removal of `arrival` land in one change. |
| ADR 002 D1 | Branch: the arrivals belong to one line's turn. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | Dependencies point outward: the owner injects the branch's dispatcher and timeline, and no core part depends on it; the Session tool part consumes it. |
| ADR 002 D4 | No configuration of its own: the timeout is the call's parameter, not a setting. |
| ADR 002 D5 | The contract is language-neutral — a task id, a timeout, a result kind, the task ids and an admission time; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None. The access by id is [ADR 027](../../adr/027-cross-scope-access-by-id.md) D1, tracked by [#2127](https://github.com/arcships/dim/issues/2127); the client path is the Rpc owner's addition, tracked by [#1659](https://github.com/arcships/dim/issues/1659).

## Binding appendix

The service is marked on the `Wait` type; the wire name stays `wait`.

```rust
use crate::host::CancelledFn;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitState {
    pub waiting: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(
    tag = "reason",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WaitResult {
    Notification {
        task_ids: Vec<String>,
        waited_ms: u64,
    },
    UserSteer {
        waited_ms: u64,
    },
    AlreadyNotified {
        task_ids: Vec<String>,
        waited_ms: u64,
    },
    Timeout {
        waited_ms: u64,
    },
    Interrupted {
        waited_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WaitCallError {
    #[error("wait is already running in this session.")]
    AlreadyWaiting,
    #[error("no turn is open")]
    NoTurn,
    #[error("the committed head cannot be read")]
    Unavailable,
    #[error("capability reloading")]
    Reloading,
}

#[rutis::service]
impl Wait {
    pub async fn state(&self) -> Result<WaitState, WaitCallError> { /* .. */ }
    pub async fn wait(&self, request: WaitRequest, cancelled: CancelledFn) -> Result<WaitResult, WaitCallError> { /* .. */ }
}
```
