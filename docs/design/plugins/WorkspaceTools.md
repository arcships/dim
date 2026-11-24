# WorkspaceTools

Scope: session (WorkspaceTools), process (WorkspaceToolsOwner, MentionIndex). Issue: #2026.

WorkspaceTools owns the Session's host tools: `read`, `write`, `edit`, `glob`, `grep`, `exec` and `exec_stop`, the method namespaces that read their output, and the process owner that holds the mention index and the file-write coordinator. The current implementation is described in the [WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md). FileReads and BackgroundExec are branch owners (#2028); their calls appear only as requirements.

## Use and scope: responsibility split

WorkspaceTools owns:

- **The host tool table.** The declarations of the seven tools and their execution, with the Session's cwd, output directory and boundaries.
- **File I/O.** Reads, globs, greps, the staged write and edit replacements, and the per-path write admission the owner keeps across Sessions and generations.
- **Command execution.** Foreground commands and their process trees; a command handed to the background is BackgroundExec's (#2028).
- **Output registration.** An output path a result gives the model is registered with the Session's file owner before the result leaves (#2018).
- **The mention index.** One process-wide walked directory index, shared by the Session search and the management search, reused for ten seconds per root.
- **The four method namespaces.** `session.files`, `session.exec.pending`, `session.exec.finished`, `session.exec.output` and `.page`.

Callers keep:

- Approval and permission: ToolExecutor and Permission decide; the host tools never decide whether a call may run.
- Which branch state a file call needs: FileReads and BackgroundExec own it; this owner reaches the line by id.
- The model-facing text: declarations are this owner's, their policy sentences are judged by #2027's rule.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The tool table reads the host tools' declarations | No: the declarations are built from the Session's configuration | `declarations` |
| The branch executes one host tool call | No: the process tree, the write admission and the output handoff are the owner's | `execute` |
| The branch resolves a write target before admission | No: path resolution and the boundary are the owner's | `resolve_write` |
| The branch orders a call within its batch | No: the class is a property of the call's tool | `ordering` |
| A client reads the files an `@` mention offers | No: the walked index and its freshness are the owner's | `search` |
| A client reads a line's command output | No: the output file is registered by the line's BackgroundExec | `session.exec.output` reached by line id |
| Register an output path with the Session's file owner | Yes: the tool does it before returning | Business inside the owner |

An operation or parameter exists only where the row says No.

## Caller example

ToolRegistry injects the host tools owner and reads the table at apply:

```rust
let tools = host.declarations();
// ToolRegistry registers each host tool with its execute and a no-op claim; the host owner's table is read, not handed over.
```

The registry runs a host tool call, and resolves a write target before admission:

```rust
let result = host.execute(call, caller, cancelled.clone()).await?;
let call = host.resolve_write(call, cancelled.clone()).await?;
let class = host.ordering(call)?;
```

A client searches, and the Session part reads its own line's task:

```rust
let SearchReply { candidates } = index.search(root, query, limit, cancelled.clone()).await?;
let ExecOutput { text } = line_owner::<BackgroundExec>(line)?.output(task_id).await?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Home, shell, environment and process supervision. |
| `S:sessionScope` | `config` for cwd, the collaboration role and the output directory, `identity` for the Session id and the search root, and `activated`/`closing`/`hold` for the tool generation and a running background task. |
| `S:sessionMethods` | Register the four method namespaces until the part unloads. |
| `P:mentions` | The process owner's walked index. |
| `P:sessionFiles` | Register an output path with the Session's file owner. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `declarations` | The host tools' declarations for this Session | `{}` | `{tools}` | None | None |
| `execute` | Run one host tool call | `{call, caller}` | `{result}` | The caller's token stops the command or the call; a finished file write is not undone | `invalid`, `cancelled`, `closed` |
| `resolve_write` | Resolve a write or edit target without effects, before admission | `{call}` | `{call}` | The caller's token stops the resolution | `invalid`, `cancelled`, `closed` |
| `ordering` | The class deciding whether the call shares its step | `{call}` | `{class: "concurrent" \| "exclusive"}` | None | `invalid`, `closed` |
| `search` | The files an `@` mention offers under one root | `{root, query, limit?}` | `{candidates: [{relativePath, absolutePath, dir}]}` | The caller's token stops the search; the walk ends at its budget | `invalid`, `cancelled`, `closed` |
| `session.files.search` | The Session's own workspace search | `{sessionId, query, limit?}` | `{candidates}` | The caller's token | `invalid`, `cancelled`, `closed` |
| `session.exec.pending` | A line's pending commands at a cutoff | `{sessionId, branchId, through?, after?}` | `{branchId, through, tasks, next}` | The caller's token stops the read | `invalid`, `cancelled`, `closed` |
| `session.exec.finished` | A line's finished commands at a cutoff | `{sessionId, branchId, through?}` | `{branchId, through, tasks}` | The caller's token stops the read | `invalid`, `cancelled`, `closed` |
| `session.exec.output` | A task's bounded live tail | `{sessionId, branchId, taskId}` | `{text}` | None | `invalid`, `missing`, `closed` |
| `session.exec.output.page` | One page of a task's registered output file | `{sessionId, branchId, taskId, cursor, limit?}` | `{text, bytes, nextCursor?, truncated}` | The caller's token | `invalid`, `missing`, `unavailable`, `closed` |

`caller` is `{turnId, requestId, branchId}` data. `session.files.search` and the management `workspace.files.search` call the same `search` with the same index; the management entry supplies its own root and the Session entry supplies the Session's cwd. Paths cross as UTF-8 strings.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The call names no host tool, the root is not an absolute existing directory, the task id or cursor is malformed, or the write target cannot be resolved |
| `missing` | The task is not known on this line; list the line's tasks |
| `unavailable` | The task's output file is gone; treat the output as unavailable |
| `cancelled` | The call stopped before its effect; nothing was written or started |
| `closed` | This Session or line is closing; a later generation serves the next Session |

The `session.exec.*` methods keep the client's codes: `missing` is `exec_task_unknown` and `unavailable` is `exec_output_unavailable`; `invalid` is `invalid_params`.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `execute` | Resolve cwd and boundary, run or write, register the output path, return the result | The process tree or the write stops; a published write stays | The call ends; a background handoff settles with BackgroundExec | New calls are refused; in-flight calls end `closed`; running commands of the part stop | WorkspaceTools for the call and the process tree; BackgroundExec for a handed-off command |
| `resolve_write` | Resolve the path against the boundary, answer the call | The resolution stops | Nothing to release | Refused `closed` | WorkspaceTools |
| `ordering` | Read the call's tool class | — | Nothing | Refused `closed` | WorkspaceTools |
| `search` | Walk or reuse the root's index, rank, answer UTF-8 paths | The search stops; the walk is the owner's and ends at its budget | Nothing to release | Refused `closed` | WorkspaceToolsOwner |
| `session.exec.output*` | Resolve the line, ask its BackgroundExec | The read stops | Nothing | Answers by the line's state | BackgroundExec |

## Exceptions with reasons

- A file write publishes before its result is returned: a cancel after the replace leaves the file changed and the result says so; the declaration on the result is the fact consumers read.
- The output path is registered with `sessionFiles` before the result leaves: a registration failure fails the call, so the model never receives an unowned path.
- A collaboration member keeps commands in the foreground: the leader treats a member's idle as the end of a request, so a background command would finish after its result.
- The file-write coordinator outlives the part and every Session it serves: old calls settle in the domain new ones enter.

## Verification

- The declarations match the Session's cwd, boundary and mount list; a call in flight when the part unloads ends `closed`.
- `execute` runs each tool, registers an output path before returning, and hands a background command to the line's BackgroundExec by id.
- `resolve_write` answers the same call with an absolute canonical path and writes nothing.
- The Session search and the management search share one walk; a file written after a walk is not offered until the next walk; paths are UTF-8 strings.
- `session.exec.output` answers a live task's tail and `missing` for an ended or unknown task; `.page` reads the registered file by cursor.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part and one process owner; neither writes a Timeline fact, so neither is a Capability. BackgroundExec is the branch Capability (#2028). |
| §2 scopes | The tools, the output directory and the methods are Session; the index and the write coordinator are process; branch state is reached by line id through `line_owner::<T>(line)`, which may call any branch data face except registration points and lifecycle services. |
| §3 orthogonality | No other owner's state is read; the line's owners are reached by id. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact of this owner. |
| §4 rule 6 | Results reach the model through ToolExecutor; this owner adds no context path. |
| §4 rule 7 | Callers use these operations; no caller reads the table or the coordinator. |
| §4 rule 8 | No infrastructure change; HostPlatform's shape is #2032's. |
| §7 values, ids and callbacks | Declarations, calls and results are data; the line and the task are ids; the index hands out candidates, not handles. |
| §7 no proxy | The Session search and the management search both call this owner's `search`; the App Server does not copy the index. |
| §7 one interface | `toolHost` is marked on the host tools owner; `ToolHost`/`ToolDriver` wrappers leave; the methods stay on `sessionMethods`. |
| §7 cross-language | Every operation is data; no method exists only for Rust. |
| §7 one change | The implementation task deletes the wrappers and the `TreeCommands` lookups and migrates every caller in one change. |
| ADR 002 D1 | Session scope for the tools; process for the shared index and the write coordinator. |
| ADR 002 D2 | No wiring plugin: the operations are on the owners' types. |
| ADR 002 D3 | The host tools are the core; FileReads and BackgroundExec are branch owners they reach by id. |
| ADR 002 D4 | The boundary and the coordinator are assembly configuration, not business settings. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): the line access that lets this Session part reach FileReads and BackgroundExec by `(session, line)` id. The file tools need the line's read bindings and `exec` needs the line's background owner; neither may be taken from a `Branch` object.
2. [#2028](https://github.com/arcships/dim/issues/2028): FileReads' binding operation and BackgroundExec's `adopt`/`output`/`wait` operations, reached by line id; the current calls take them from `TreeCommands.get(line)?.service::<T>()`.
3. [#2018](https://github.com/arcships/dim/issues/2018): `sessionFiles.register` and the read/write operations the output and attachment paths use.

## Supersedes

- [tool-contributions §2.1](../../architecture/rust-agent/tool-contributions.md): the `toolHost` data face's `execute(call, caller, cancelled)` is joined by `declarations`, `resolve_write` and `ordering`, and `ToolDriver` no longer exists.
- [service faces](../service-faces.md): the `toolHost` and `mentionIndex` rows; `mentions.search` replaces the `MentionIndex::candidates` reader.

## Binding appendix

The host tools' face is marked on the host tools owner under `toolHost`; the index's face is marked on the process owner under `mentionIndex`. `ToolHost` and `ToolDriver` no longer exist. `CancelledFn` is the binding's cancellation argument and is not a request field; `context` is `{turnId, requestId, branchId}` data.

```rust
use crate::host::{CancelledFn, ToolExecutionContext};
use dim_agent_types::{ToolCall, ToolDefinition, ToolResult};

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ToolHostError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    Missing { message: String },
    #[error("{message}")]
    Unavailable { message: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the Session or line is closing")]
    Closed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchReply { pub candidates: Vec<Candidate> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate { pub relative_path: String, pub absolute_path: String, pub dir: bool }

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderClass { Concurrent, Exclusive }

#[rutis::service(name = "toolHost")]
impl WorkspaceTools {
    pub fn declarations(&self) -> Vec<ToolDefinition> { /* ... */ }

    pub async fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancelled: CancelledFn,
    ) -> Result<ToolResult, ToolHostError> { /* ... */ }

    pub async fn resolve_write(&self, call: ToolCall, cancelled: CancelledFn) -> Result<ToolCall, ToolHostError> { /* ... */ }

    pub fn ordering(&self, call: ToolCall) -> Result<OrderClass, ToolHostError> { /* ... */ }
}

#[rutis::service(name = "mentionIndex")]
impl WorkspaceToolsOwner {
    pub async fn search(
        &self,
        root: String,
        query: String,
        limit: Option<usize>,
        cancelled: CancelledFn,
    ) -> Result<SearchReply, ToolHostError> { /* ... */ }
}
```

`session.files.search`, `session.exec.*` and `workspace.files.search` remain method handlers registered with `sessionMethods`; they call `search` or the line's BackgroundExec.
