# BackgroundExec

Scope: branch. Issue: #2028.

BackgroundExec owns the commands a line moved to the background: the task table, each task's terminal record and wake, and the task's registered output artifact. This document is the owner's target contract; the current implementation is described in the [WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec).

## Use and scope: responsibility split

BackgroundExec owns:

- **The task table.** One task per background command of the line: the task id, its description and title, the registered output path, the process it supervises, and its terminal record.
- **Supervision.** Wait for the process the host started, write `exec/ended` with its status and result, hold the Session until the terminal fact is appended, and wake the line with a bounded notice unless the end came from a stop, a branch close or an unload.
- **The output artifact.** The task's registered output file: the tail preview and its bounded pages, the truncation flag, and the unavailable answer when the file is gone. The bytes are appended by the process; the artifact reference and every read belong to this owner.
- **Recovery.** On load, write failed terminals for commands whose process did not survive, and post the wakes the previous generation lost.

BackgroundExec does not create, start or kill processes. The host's process owner starts a command and issues its task id; BackgroundExec adopts the id, and stops the task by asking the host to kill that id. ShellSession owns the PTY and its own stop port; its commands' terminals are settled by the ShellSession owner through `exec/started`/`exec/ended`, and this owner consumes them.

Callers keep as business:

- Whether a command is moved to the background: the exec tool decides after its foreground wait.
- What a completion notice means for the turn.
- The command, its working directory and its environment: the host's process owner builds the process.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Supervise a command the host started | No: the supervision task, the terminal fact and the Session hold are one lifecycle; a caller holding the process would duplicate the settlement on every path | `adopt` |
| Stop a task by id | No: the stop must cancel the supervision, kill the process and write one terminal without a wake; a caller composing the steps could write two terminals | `stop` |
| Read the task's tail preview | No: the artifact is the owner's record and the read is bounded by the owner's policy | `output` |
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
}).await?; // Closed, UnknownTask
```

The product reads the task's preview and pages:

```rust
let preview = background.output(task_id.clone()).await?; // Unknown, Unavailable, Closed
let page = background.output_page(OutputPageRequest {
    task_id,
    cursor,
    limit: 64 * 1024,
}).await?; // Unknown, Unavailable, InvalidLimit, Closed
```

A client stops a task and waits for its terminal:

```rust
let ended = background.stop(task_id.clone()).await?; // Unknown, Closed
let ended = background.wait(WaitRequest { task_id, timeout_ms: 5_000 }, cancelled).await?; // Unknown, Timeout, Closed
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` (process, #2032) | The started process by task id: wait for it, kill it, and answer its terminal status. |
| `S:SessionScope` (Session, #2093) | Hold the Session while a task runs; observe activation and closing. |
| `S:GuideEntries` (Session) | Register the exec notification's entry for this line until the part unloads. |
| `B:Timeline`, `B:Dispatcher` | Write and recover `exec/started`/`exec/ended`; consume terminals and interrupts; write the wake input. |

The host's process-by-id operations are not defined yet; they are stated in Requirements.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `adopt` | Take over supervision of a command the host started | `{taskId, description, title?, outputPath}` | `{}` | The caller drops the call: nothing was adopted, and the host's process is killed | `closed`, `unknown_task` (the host has no live task with this id) |
| `output` | The task's tail preview | `{taskId}` | `{text}` | The caller drops the call; the read stops | `unknown`, `unavailable`, `closed` |
| `output_page` | One bounded page of the registered output file | `{taskId, cursor, limit}` | `{text, bytes, nextCursor?, truncated}` | The caller drops the call; the read stops | `unknown`, `unavailable`, `invalid_limit`, `closed` |
| `wait` | Wait until the task's terminal is dispatched | `{taskId, timeoutMs}` | `{terminal}` | The caller drops the call; the wait releases | `unknown`, `timeout`, `closed` |
| `stop` | Cancel a running task and answer its terminal | `{taskId}` | `{terminal}` | None: an admitted stop settles | `unknown`, `closed` |

`nextCursor` is present only for a full page; a client advances by `bytes`. `wait` with `timeout_ms = 0` answers `timeout` for a running task and the terminal for an ended one. `stop` on an ended task answers its terminal and changes no wake.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `unknown_task` | `adopt` | The host has no such task; nothing was adopted and the command is not supervised. |
| `unknown` | `output`, `output_page`, `wait`, `stop` | This line has no such task; do not retry. |
| `unavailable` | `output`, `output_page` | The registered file is gone (cleaned or its Session deleted); report the loss, do not retry. |
| `invalid_limit` | `output_page` | Use a limit of at least one byte. |
| `timeout` | `wait` | The task is still running; wait again or stop it. |
| `closed` | any | The generation is closing or reloading; a replacement generation serves. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `adopt` | Take the Session hold, write `exec/started`, spawn the supervision task, answer the id | Before `exec/started` is committed: nothing adopted; the host's process is killed | Nothing to release; the task is the line's | A reload refuses new adoptions and ends running tasks as stopped; an unfinished command gets a failed terminal | BackgroundExec for the task and the hold; the host for the process |
| supervision task | The host reports the process's end; write `exec/ended`, release the Session hold, wake the line unless the end is `cancelled` | — | — | The owner's unload cancels the task, kills the process and writes the cancelled terminal | BackgroundExec for the facts; the host for the process |
| `output`, `output_page` | Open the registered file, read the tail or one page, answer | The read stops; nothing changes | — | Pending reads answer `closed` | BackgroundExec |
| `wait` | Answer at once for an ended task; otherwise wait for the dispatched terminal | The wait releases; no state changes | — | Pending waits answer `closed` | BackgroundExec |
| `stop` | Cancel the supervision, ask the host to kill the process, write the cancelled terminal, answer it | — | — | A stop in flight settles before the generation closes | BackgroundExec for the terminal; the host for the process |

## Exceptions with reasons

- The registered output file's bytes are produced by the process, while the artifact reference and every read belong to this owner: the path is fixed in `exec/started`, the process only appends, and a read opens and closes within one call, so no file handle crosses an owner.
- ShellSession's commands appear in `exec/started` without an adopting caller: the PTY owner settles its own commands and writes their terminals; this owner consumes them for the notices and recovery, as it does for its own tasks.

## Verification

Flow tests on the owner:

- A command moved to the background ends once, its terminal is written once, and the line wakes once; a stop cancels it without a wake; a branch close cancels it without a wake.
- A reload writes failed terminals for unfinished commands, posts the wakes the previous generation lost, and refuses a new adoption; a Session close settles the hold.
- The tail preview and its pages read the registered file in every state of the task; a page's cursor advances by its bytes; a limit of zero is `invalid_limit`; a deleted file is `unavailable`.
- `wait` answers an ended task at once, times out for a running task, and releases when dropped; an unknown task is `unknown` on every operation.
- The face and the Rust call answer the same previews, pages, terminals and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | BackgroundExec is one branch part and a Capability: it injects `dispatcher` and `timeline`, writes `exec/ended` and folds the terminals. |
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

## Requirements raised to mechanism owners

- **HostPlatform (#2032).** The process owner must expose a started process by task id: `wait({taskId}, cancelled) → {status, exit}`, `kill({taskId}) → {}`, and the task id in its start reply. BackgroundExec adopts that id and never receives a process object. Until the host's id operations land, this board's `adopt` and `stop` cannot be implemented; the other owners' contracts are independent.
- **SessionScope (#2093).** `hold` returns the lease id, `release` ends it early, and `activated`/`closing` gate the watcher.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `backgroundExec`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::{ExecEndedData, ExecStatus};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptRequest {
    pub task_id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub output_path: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct WaitRequest {
    pub task_id: String,
    pub timeout_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum BackgroundError {
    #[error("the host has no live task with this id")]
    UnknownTask,
    #[error("no such task is known on this line")]
    Unknown,
    #[error("the task's output file is no longer available")]
    Unavailable,
    #[error("limit must be at least 1")]
    InvalidLimit,
    #[error("the task did not end before the timeout")]
    Timeout,
    #[error("the background exec service is closing")]
    Closed,
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
