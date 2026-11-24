# BackgroundExec

Scope: branch. Issue: #2028.

BackgroundExec owns the commands a line moved to the background: the task table, each task's terminal record and wake, and the task's registered output artifact. This document is the owner's target contract; the current implementation is described in the [WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec).

## Use and scope: responsibility split

BackgroundExec owns:

- **The task table.** One task per background command of the line: the task id, its description and title, the registered output path, the process it supervises, and its terminal record. On load, the table is rebuilt from the line's `exec/started` and `exec/ended` facts at one cutoff; a task with a start and no end is written a failed terminal, and every ended task's id, description, title, output path and status come back, so historical reads answer without a live process.
- **Supervision.** Adopt the process the host started, wait for it, write `exec/ended` with its status and result, hold the Session until the terminal fact is appended, and wake the line with a bounded notice unless the end came from a stop, a branch close or an unload.
- **The output artifact.** The task's registered output file: the live task's tail preview, the bounded page read, the truncation flag, and the unavailable answer when the file is gone. The bytes are appended by the process; the artifact reference and every read belong to this owner.

BackgroundExec does not create, start or kill processes. The host's process owner starts a command and issues its task id; BackgroundExec adopts the id, and stops the task by asking the host to kill that id. ShellSession owns the PTY and its own stop port; its commands' terminals are settled by the ShellSession owner through `exec/started`/`exec/ended`, and this owner consumes them.

Targets: the host's process operations are a target, pending [#2032](https://github.com/arcships/dim/issues/2032); the Session hold is a target, pending [#2093](https://github.com/arcships/dim/pull/2093); the line access is [ADR 027](https://github.com/arcships/dim/pull/2029) with the mechanism obligation [#2127](https://github.com/arcships/dim/issues/2127).

Callers keep as business:

- Whether a command is moved to the background: the exec tool decides after its foreground wait.
- What a completion notice means for the turn.
- The command, its working directory and its environment: the host's process owner builds the process.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Supervise a command the host started | No: the supervision task, the terminal fact and the Session hold are one lifecycle; a caller holding the process would duplicate the settlement on every path | `adopt` |
| Stop a task by id | No: the stop must cancel the supervision, kill the process and write one terminal without a wake; a caller composing the steps could write two terminals | `stop` |
| Read a running task's tail preview | No: the artifact is the owner's record and the read is bounded by the owner's policy | `output` |
| Read the task's output in pages | No: the page's cursor, the character boundary and the truncation flag are the owner's knowledge | `output_page` |
| Wait until a task ends | No: the wait must observe the dispatched terminal, not poll the record | `wait` |

An operation exists only where its row says No.

## Caller example

The exec tool moves a foreground command to the background and answers its task id:

```rust
// The host's process owner started the command and issued its task id.
let task_id = launch.task_id.clone();
background.adopt(AdoptRequest {
    task_id: task_id.clone(),
    description: request.description.clone(),
    title: request.title.clone(),
    output_path: launch.output_path.clone(),
}).await?; // Closed, UnknownTask, Failed
```

The product reads the running task's preview and pages its file:

```rust
let preview = background.output(task_id.clone()).await?; // Unknown, Closed; an ended task is unknown here
let page = background.output_page(OutputPageRequest {
    task_id,
    cursor,
    limit: 64 * 1024,
}).await?; // Unknown, Unavailable, InvalidLimit, Read, Closed
```

A client stops a task and waits for its terminal:

```rust
let ended = background.stop(task_id.clone()).await?; // Unknown, Read, Closed
let ended = background.wait(WaitRequest { task_id, timeout_ms: 5_000 }, cancelled).await?; // Unknown, Timeout, Read, Closed, Cancelled
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` (process, target, pending [#2032](https://github.com/arcships/dim/issues/2032)) | The started process by task id: wait for it, kill it, and answer its terminal status. |
| `S:SessionScope` (Session, target, pending [#2093](https://github.com/arcships/dim/pull/2093)) | Hold the Session while a task runs; observe activation and closing. |
| `S:GuideEntries` (Session) | Register the exec notification's entry for this line until the part unloads. |
| `B:Timeline`, `B:Dispatcher` | Write and recover `exec/started`/`exec/ended`; consume terminals and interrupts; write the wake input. |

The host's process-by-id operations are a target of #2032; they are stated in Requirements.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `adopt` | Take over supervision of a command the host started | `{taskId, description, title?, outputPath}` | `()` | The caller drops the call: nothing was adopted, and the host's process is killed | `closed`, `unknown_task` (the host has no live task with this id), `failed` (the `exec/started` fact could not be committed; nothing was adopted and the process is killed) |
| `output` | The running task's tail preview | `{taskId}` | `String` | The caller drops the call; the read stops | `unknown` (no such task, or the task has ended), `closed` |
| `output_page` | One bounded page of the registered output file; a stateless offset read, with no pin or release | `{taskId, cursor, limit}` | `OutputPage` | The caller drops the call; the read stops | `unknown`, `unavailable`, `invalid_limit`, `read` (the task facts could not be read or are inconsistent), `closed` |
| `wait` | Wait until the task's terminal is dispatched | `{taskId, timeoutMs}` | `ExecEndedData` | The caller drops the call; the wait releases and nothing changes | `unknown`, `timeout`, `read`, `closed`, `cancelled` |
| `stop` | Cancel a running task and answer its terminal | `{taskId}` | `ExecEndedData` | None: an admitted stop settles | `unknown`, `read`, `closed` |

`next_cursor` is present only for a full page; a client advances by `bytes`. `wait` with `timeout_ms = 0` answers `timeout` for a running task and the terminal for an ended one. `stop` on an ended task answers its terminal and changes no wake. Replies are positional.

### Errors

| Kind | Operation | Retryable | What the caller does next |
| --- | --- | --- | --- |
| `unknown_task` | `adopt` | No | The host has no such task; nothing was adopted and the command is not supervised. |
| `unknown` | `output`, `output_page`, `wait`, `stop` | No | This line has no such task; do not retry. `output` also answers `unknown` for an ended task, whose file stays readable by `output_page`. |
| `unavailable` | `output_page` | No | The registered file is gone (cleaned or its Session deleted); report the loss, do not retry. |
| `invalid_limit` | `output_page` | No | Use a limit of at least one byte. |
| `read` | `output_page`, `wait`, `stop` | Yes | The task facts could not be read or are inconsistent; retry the read; if it persists, the task's record is damaged and the caller reports a fault. |
| `timeout` | `wait` | Yes | The task is still running; wait again or stop it. |
| `failed` | `adopt` | Yes, by the exec tool retrying the launch | The `exec/started` fact could not be committed; nothing was adopted; the process is killed. |
| `cancelled` | `wait` | No | The caller's token ended the wait; nothing changed. |
| `closed` | any | No, in this generation | The generation is closing or reloading; a replacement generation serves. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `adopt` | Take the Session hold, write `exec/started`, spawn the supervision task, answer the id | Before `exec/started` is committed: nothing adopted; the host's process is killed | Nothing to release; the task is the line's | A reload refuses new adoptions and ends running tasks as stopped; an unfinished command gets a failed terminal | BackgroundExec for the task and the hold; the host for the process |
| supervision task | The host reports the process's end; write `exec/ended`, release the Session hold, wake the line unless the end is `cancelled` | — | — | The owner's unload cancels the task, kills the process and writes the cancelled terminal | BackgroundExec for the facts; the host for the process |
| `output` | Answer the running task's artifact tail; an ended task is `unknown` | The read stops; nothing changes | — | Pending reads answer `closed` | BackgroundExec |
| `output_page` | Open the registered file, read one page at the cursor, answer; works for running, ended and reloaded tasks | The read stops; nothing changes | — | Pending reads answer `closed` | BackgroundExec |
| `wait` | Answer at once for an ended task; otherwise wait for the dispatched terminal | The wait releases; no state changes | — | Pending waits answer `closed` | BackgroundExec |
| `stop` | Cancel the supervision, ask the host to kill the process, write the cancelled terminal, answer it | — | — | A stop in flight settles before the generation closes | BackgroundExec for the terminal; the host for the process |
| Load | Fold `exec/started` and `exec/ended` at one cutoff into the task table; write failed terminals for unfinished commands; post the wakes the previous generation lost | — | — | — | BackgroundExec |

## Client-visible changes

The published `backgroundExec` face ([WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md#data-face)) keeps `output`, `output_page`, `wait` and `stop` with their published shapes, and `start` stays off the face. The changes, which the maintainer is asked to confirm:

- `adopt({taskId, description, title?, outputPath})` is new (additive); the exec tool uses it after the host starts the process.
- Error codes change case with the standard binding spelling: `invalidLimit` becomes `invalid_limit`; `read` is kept; `unknown_task`, `failed` and `cancelled` are new.
- `output` keeps its published promise: a running task's live tail preview, `unknown` once the task ends; `output_page` keeps reading the registered file for every state.
- No client RPC, notification name, request field or model-visible text changes.

## Exceptions with reasons

- The registered output file's bytes are produced by the process, while the artifact reference and every read belong to this owner: the path is fixed in `exec/started`, the process only appends, and a read opens and closes within one call, so no file handle crosses an owner.
- Each command's `exec/started` and `exec/ended` have exactly one writer: this owner for the tasks it adopts, and ShellSession for its PTY commands, which appear in `exec/started` without an adopting caller; this owner consumes those terminals for the notices and recovery.
- A historical task's `output_page`, `wait` and `stop` answer from the rebuilt table and the registered file; no historical task cache is kept beyond the table.

## Supersedes

- `docs/design/service-faces.md` §3.4's `backgroundExec` row and the sentence about `backgroundExec.start`.
- `docs/design/background-task-output.md`'s `TreeCommands` lookup paragraph (the task read now goes by line id).
- The current implementation page (`docs/architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec`) keeps describing the implementation until this contract lands.

## Verification

Flow tests on the owner:

- A command moved to the background ends once, its terminal is written once, and the line wakes once; a stop cancels it without a wake; a branch close cancels it without a wake.
- A reload writes failed terminals for unfinished commands, rebuilds every ended task with its output path, posts the wakes the previous generation lost, and refuses a new adoption; a Session close settles the hold.
- A historical task answers `output_page` and `stop` after a reload; `output` is `unknown` for it; `wait` answers its terminal at once.
- The tail preview and the pages read the artifact in every state of the task; a page's cursor advances by its bytes; a limit of zero is `invalid_limit`; a deleted file is `unavailable`; a failed task-fact read is `read`.
- `wait` answers an ended task at once, times out for a running task, and releases when dropped or cancelled; `adopt` answers `unknown_task` for an id the host does not have and `unknown` answers the other operations for a task this line does not know.
- The face and the Rust call answer the same previews, pages, terminals and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BackgroundExec is one branch part and a Capability: it injects `dispatcher` and `timeline`, writes its adopted tasks' `exec/started`/`exec/ended` and folds them at load; ShellSession writes its own commands' facts, as the exception states. |
| §2 scopes | Branch: a task belongs to the line that started it; the process belongs to the host's process owner. |
| §3 orthogonality | The host's process, the Session's hold and the guide table are other owners' operations; no state is read directly. |
| §4 rule 1 | A stop and a close cancel the task; the owner writes the terminal. |
| §4 rule 2 | The process is handed over by id; the task is not taken back. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | The terminal and its wake are separate commits: the terminal first, the wake after its dispatch. |
| §4 rule 6 | The notice is the model's input; the full output stays in the file and the result. |
| §4 rule 7 | The host's process and the Session's hold are operations; the guide entry is a registration. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | The terminal is a fact later decisions use; the notice is an input. |
| §7 resources and ids | The process is the host's, addressed by task id; the task's output artifact is the owner's; no handle crosses. |
| §7 no proxy | The owner serves its own tasks and their artifact; it does not return the host's process state. |
| §7 one surface | `adopt`, `output`, `output_page`, `wait`, `stop`; `start` with a live process is removed. |
| §7 cross-language | Every parameter and reply is data; cancellation is dropping the call. |
| §7 responsibility split | The caller keeps the command and the decision to background it; the owner keeps supervision, the terminal, the hold and the artifact. |
| ADR 002 D1 | Branch scope. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | Depends on the host's process owner and Session capabilities; nothing depends on BackgroundExec's internals. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Language-neutral operations and data. |
| ADR 027 | The Session's `session.exec.output*` methods reach a line's BackgroundExec by line id (`line_owner::<BackgroundExec>(line)`); the task id stays in the request. |

## Requirements raised to mechanism owners

- **HostPlatform ([#2032](https://github.com/arcships/dim/issues/2032), target).** The process owner must expose a started process by task id: `spawn(...) → {taskId}`, `wait({taskId}, cancelled)`, `kill({taskId})`, `settle({taskId})`, and settle its task table on close or reload. BackgroundExec adopts that id and never receives a process object. Until the host's id operations land, this board's `adopt` and `stop` cannot be implemented; the other owners' contracts are independent.
- **SessionScope ([#2093](https://github.com/arcships/dim/pull/2093), target).** `hold` returns the lease id, `release` ends it early, and `activated`/`closing` gate the watcher.
- **Cross-scope mechanism ([#2127](https://github.com/arcships/dim/issues/2127), target).** The Session's exec output methods reach a line's owner by line id per ADR 027; this owner states the operations, not the mechanism.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `backgroundExec`. `ExecEndedData` is a `dim_agent_types` value.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::ExecEndedData;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptRequest {
    pub task_id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub output_path: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputPageRequest {
    pub task_id: String,
    pub cursor: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPage {
    pub text: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitRequest {
    pub task_id: String,
    pub timeout_ms: u64,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BackgroundError {
    #[error("the host has no live task with this id")]
    UnknownTask,
    #[error("no such task is known on this line")]
    Unknown,
    #[error("the task's output file is no longer available")]
    Unavailable,
    #[error("limit must be at least 1")]
    InvalidLimit,
    #[error("the task facts could not be read: {reason}")]
    Read { reason: String },
    #[error("the task did not end before the timeout")]
    Timeout,
    #[error("the background exec service is closing")]
    Closed,
    #[error("the task could not be adopted: {reason}")]
    Failed { reason: String },
    #[error("the wait was cancelled")]
    Cancelled,
}

impl PublicService for BackgroundExec {}

#[rutis::service(name = "backgroundExec")]
impl BackgroundExec {
    pub async fn adopt(&self, request: AdoptRequest) -> Result<(), BackgroundError> { /* ... */ }
    pub async fn output(&self, task_id: String) -> Result<String, BackgroundError> { /* ... */ }
    pub async fn output_page(&self, request: OutputPageRequest) -> Result<OutputPage, BackgroundError> { /* ... */ }
    pub async fn wait(&self, request: WaitRequest, cancelled: CancelledFn) -> Result<ExecEndedData, BackgroundError> { /* ... */ }
    pub async fn stop(&self, task_id: String) -> Result<ExecEndedData, BackgroundError> { /* ... */ }
}
```
