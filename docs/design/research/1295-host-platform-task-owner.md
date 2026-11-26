# #1295 clean redesign: one task owner, one record, one splitter
Status: design and implementation record for #1295 from `984f6b59d1`; the earlier
`af6752a2e9` patch stack is only the local `backup/pr-2002-af675` reference. The contract
lands in `docs/design/resource-faces.md` §3; other pages link to it.
## 0. Decision
1. `HostPlatform` owns all four task kinds: exec command, persistent-shell command, shell
   job, persistent shell. One `TaskRecord` with an immutable `TaskKind` payload makes
   per-kind state unrepresentable elsewhere. `ShellSession` owns policy; the tool owners
   hold task ids.
2. One terminal watch, one splitter, one `OutputPage`, one `HostPlatformError`, one typed
   `TaskFailure`. Every added field has a consumer and a test.
3. The public shell event stream is deleted, approved by the user/PR owner (§3.2);
   terminal records plus `output`/`tail`/`wait`/`retainOutput` replace it.
4. Service methods take `&self`; no `mounted` weak, no `arc()`, no test-only path.
5. Paths are UTF-8 `String`; errors are typed enums whose tags are the codes.
6. Seven migration steps; no step waits for a product approval. `session.exec.stop` is
   routed in this PR through the branch stop owner, without adding a `ShellSession`
   dependency to `BackgroundExec`.
## 1. Ownership and state
`HostPlatform` owns every process/PTY/task and its terminal, timeout, isolation marker and
log. `ShellSession` owns queue/timeout/occupancy/markers/facts; `WorkspaceTools`/
`BackgroundExec` own the branch id, result and registered path. The branch writes the
facts; the owner writes none. `ShellInput` is unchanged.
### 1.2 One record, per-kind payload
```text
reserve(id, log)               process started / wrapper written / PTY spawned
      ▼                                        ▼
   Reserved ────────── start failure: remove ──► Running ──────────► Terminal(term)
                                                   └─ kill / timeout / exit / sweep
```

```rust
struct TaskRecord { id: String, started: Instant,
    state: Mutex<TaskState>, // Reserved | Running | Terminal(Arc<Terminal>)
    changed: watch::Sender<Option<Arc<Terminal>>>, kind: TaskKind }
enum TaskKind { Command(CommandTask), ShellCommand(ShellCommand), Job(JobTask), Shell(ShellTask) }
struct Terminal { status: TaskStatus, exit_code: Option<i32>, signal: Option<String>,
    duration_ms: u64, failure: Option<TaskFailure>, output: OutputRecord, note: Option<String> }
```

`CommandTask`: tree, capture, isolation, timeout. `ShellCommand`: `Arc<ShellTask>`, markers,
capture. `JobTask`: `Arc<ShellTask>`, markers, paths, pid, stop. `ShellTask`: PTY, reader,
display, log, `Weak<TaskRecord>` active/children. Only `ShellTask` has an active slot, only
`Command`/`ShellCommand` have inline text, only `Job` has a pid; shell holds `Weak`, child
holds `Arc`, so no cycle. The owner's active slot is authoritative for `kill`: `ShellSession.occupying`
is a client-side projection and never changes which record `kill` may interrupt; when they
disagree, the owner wins and the session drops the stale occupancy. `finish(terminal)` returns if already terminal, stores the `Arc`
in `state` and `changed` (wakes all), then clears active/sweeps; only `LogRecord.retained`
and the terminal mode remain. The `Terminal` (including its `OutputRecord`) is
immutable; `retainOutput` never mutates it (see §2.2).
### 1.3 Settlement
`settle(taskId)` is the only release. Terminal → remove; Reserved/Running →
`task_running`; absent or already settled → `Ok`. `kill` never releases. `kill`
on a `Reserved` record answers `task_running` and does not cancel the start; `kill` on a
terminal record is a no-op.

| Action | Exec command | Shell command | Job | Shell |
| --- | --- | --- | --- | --- |
| `kill` running | cancel + terminate; await release; `cancelled` or `failed{terminate_failed}` | ETX only if active; terminal from marker or sweep | signal pid; pending stop until release; `cancelled` on release, `failed{shell_exited}`/`failed{shell_reset}`/`failed{owner_unloaded}` on sweep; missing pid answers `task_running` and keeps a pid watcher | terminate PTY; `cancelled` or `failed{terminate_failed}`; sweep |
| `kill` terminal / settle | no-op / after result | no-op / after result | no-op / after fact | no-op / removes the shell record |
| shell exit | n/a | `failed{shell_exited}` with partial output | `failed{shell_exited}` with file read | `exited`/`signaled`/`cancelled` |
| timeout / unload | `spawn.timeout` → `timed_out`; `wait` never kills; unload `cancelled{owner_unloaded}` | deadline is `kill`; unload same | no timeout; unload same after sweep | no timeout; unload same |

A shell's terminal marks every child terminal; `settle(shellId)` removes only the shell
record. A job whose release is unconfirmed stays Running until release or the shell's sweep. `settle` keeps registered logs; the shell log exposed by the `shell`
reply is registered and kept until `removeOutputDirectory` or Session deletion; only the
internal init log is removed on release.
### 1.4 Interrupt, pid, pwsh, Windows
`kill(commandId)` checks the shell active slot and non-terminal under its lock, then writes
ETX; Reserved answers `task_running`, and a terminal, superseded or repeated interrupt is a
no-op. A shell command's terminal comes from its end marker; a shell exit sweeps it; a 2 s
expiry kills the shell. A job pid arrives through a `watch<Option<u32>>`; `kill`
subscribes before reading the pid and waits for it. The pid is accepted only when its job id
is registered and the record belongs to the same shell. A pid after terminal is ignored; a
kill before it keeps a pid watcher and answers `task_running` after 2 s; a late pid is
killed and released before the terminal. pwsh completion
writes its marker to the owner-derived `<taskId>.result`, watched for 200 ms. Windows uses the existing PTY/job
code; a stopped job is `cancelled`.
### 1.5 Wake and binding
`wait` subscribes to `changed` before reading state, so a terminal in the window is seen.
The task methods are `&self`; no `spawn_in`, no `Arc<Self>` receiver, no `Weak<Self>`. The
table, worker tracker and shutdown token are `Arc`/cloneable fields created by
`HostPlatform::current`; a test that builds it spawns the same way.
## 2. Data contract
### 2.1 Operations
| Method | Request | Reply |
| --- | --- | --- |
| `info` / `execShell` / `persistentShell` / `sandboxBackend` / `execRoot` | `{}` | facts; `execRoot` is `String`; non-UTF-8 is `invalid` |
| `spawn` / `shell` / `write` | `SpawnRequest` / `ShellRequest` / `ShellWrite` | `SpawnReply` / `SpawnReply` / `{}` |
| `resize` / `kill` / `settle` / `retainOutput` / `wait` | existing / `TaskRequest` / `WaitRequest` | `{}` / `OutputPathReply` / `WaitReply` |
| `output` / `tail` | `OutputRequest {taskId, cursor?, limit?}` / `TaskRequest` | `OutputPage` / `OutputTail` |
| `spawnWithStdin` / `spawnConfined` / `removeOutputDirectory` / `childPath` | existing / UTF-8 base | existing; `childPath` `None` = nothing to append |

`ShellWrite` is `{taskId, data, command?: {taskId, startMarker, endMarker}, job?:
{taskId, endMarker, outputPath}}`; the optional start declares the task the
written bytes begin. A job's `outputPath` must be absolute, inside the shell output directory,
with an existing parent, and its file stem must equal `taskId`. The event API is deleted (§3.2).
### 2.2 Task snapshot
| Field | Exec command | Shell command | Job | Shell |
| --- | --- | --- | --- | --- |
| `text` | complete output when the snapshot is inline; absent when the log is the source | absent | absent | absent |
| `outputPath` | data-face snapshot path; a model result exposes it only when the file exists | file | wrapper file | shell log; exposed path is registered |
| `outputBytes` / `outputTruncated` | produced; log cap, false with no file | produced; log cap | file length; false (uncapped) | produced; log cap |
| `preview`/`previewTruncated` | `bounded_preview` tail | same | bounded tail read | same |
| `failure` | `TaskFailure` | `TaskFailure` | `TaskFailure` | `TaskFailure` |
| `status` | running/exited/signaled/timed_out/cancelled/failed | running/exited/failed | running/exited/failed/cancelled | running/exited/signaled/cancelled/failed |
| `exitCode`/`signal` | when exited/signaled | numeric token; no signal | numeric token; no signal | when exited/signaled |
| `durationMs` / `failureNote` | `now - started` while running, `terminal_at - started` when terminal; isolation hint | same; absent | same; absent | same; absent |

* `text` present means the current snapshot is inline and `output` serves it; absent means
  the log is the source. A terminal snapshot is immutable. A model-facing result includes
  `outputPath` only when the file exists and is registered; the data-face `wait` may report
  the planned path for introspection, but that path is not handed to the model. `retainOutput` on a Running inline task creates the log and switches the
  capture to file mode; on a Terminal inline task it only creates a side copy, so the
  snapshot and `output` stay inline and the returned path is for a caller that wants it.

| Snapshot | `wait` | `output` | `retainOutput` |
| --- | --- | --- | --- |
| Running, inline | running + `text` | text | create log, switch to file; later snapshots are file-backed |
| Terminal, inline | terminal + `text` | text | side copy; snapshot and reads unchanged |
| File-backed | terminal/ running + path + preview | file | existing path |
* `OutputWriteFailed{message}` is the log error and forces `status: failed`, keeping
  `exitCode` if the process exited; a job read error is `JobUnavailable{message}`. A
  running snapshot has no failure/exit/signal/note. `tail`/`output` read a job file with a
  bounded seek/read; nothing loads it whole. A shell token is data: `0` →
  `exited{0}`, another numeric → `failed{code}` with `CommandFailed`, `failed` →
  `CommandFailed`, other/empty → `UnknownExit`.
### 2.3 Output page
`OutputPage = {text, bytes, totalBytes, nextCursor?, truncated, retainedFrom, reset}`.

| Case | `bytes` | `totalBytes` | `nextCursor` | `retainedFrom`/`reset` |
| --- | --- | --- | --- | --- |
| inline at EOF / cursor past end | length or 0 | text length | absent | 0/false |
| file/shell full page | ≥ `limit`, ≤ `limit+3` | total | `cursor + bytes` | 0/false (file); absolute (shell) |
| file/shell short page | bytes read | total | absent | 0/false (file); absolute (shell) |
| cursor inside a UTF-8 char | 0 or the whole first char | total | absent | unchanged |
| shell cursor below `retainedFrom` | 0 | total | absent | `retainedFrom`, `reset: true` |

`bytes` is consumed; `nextCursor` is present iff the source read reached `limit` and
`bytes > 0`; a page never splits a character; `truncated` is the source log cap.
### 2.4 Errors and failure kinds
`HostPlatformError`: `invalid`, `invalid_path`, `timed_out`, `output_limit`, `task_unknown`, `task_exists`,
`task_running`, `task_ended`, `output_unavailable`, `cancelled`, `closed`, `io`,
`isolation_config_invalid`, `isolation_unavailable`, `exec_shell_config_invalid`,
`exec_shell_unavailable`, `exec_shell_cwd_unsupported`, `cwd_not_found`,
`exec_shell_path_mapping_failed`, `failed`. One typed enum, one variant per code; the wire
tag is the code, so `xtask types` generates it. `ProcessSpecError{code,reason}` maps by an
exhaustive match; a new code fails to compile until classified. `spawnWithStdin`/
`spawnConfined` keep `InvalidPath` as `invalid_path`; `ConfinedError::TimedOut` and
`OutputLimit { stream }` keep `timed_out` and `output_limit` with the stream field; exec maps `Invalid`/the exec-shell
errors to its own codes.

`TaskFailure` is a typed enum, used identically by `Terminal.failure` and
`WaitReply.failure`:

```rust
#[derive(Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum TaskFailure {
    OutputWriteFailed { message: String }, // {"code":"output_write_failed","message":"..."}
    TerminateFailed { message: String },   // {"code":"terminate_failed","message":"..."}
    JobUnavailable { message: String },    // {"code":"job_unavailable","message":"..."}
    ShellExited, ShellReset, CommandFailed, UnknownExit, OwnerUnloaded,
}
```

`xtask types` exposes it as `TaskFailure`, a discriminated union on `code`;
`WaitReply.failure?: TaskFailure | null`. Only the three OS/read errors carry a message; a
`cancelled` record may carry `TerminateFailed`/`OwnerUnloaded`.
## 3. Shell output
### 3.1 Splitter and classification
The PTY reader is the only reader. It applies `regularize_output` once, appends the chunk
to the display capture, and feeds one splitter:

```rust
struct LineSplitter { pending: String, finished: bool }
fn push(&mut self, chunk: &str) -> Vec<Line>; // complete lines; a >64 KiB partial line is
                                              // flushed as one chunk so memory is bounded
fn finish(&mut self) -> Option<Line>;         // the remaining tail, once
```

`finish` is called exactly once at PTY EOF; its line is classified too, so a marker without
a newline lands. Classification: active command start → begin capture; active command end
→ token, terminal; registered job pid/completion → resolve once; otherwise command output
and display output. Markers are validated at `write`: `__DIM_START_<8 hex>_<8 hex>__` /
`__DIM_END_<8 hex>_<8 hex>__`, non-empty, one per line, token 1–12 ASCII alphanumerics. A
marker inside other text is not one; the wrapper splits its literal so the echo does not
match. A caller that prints its own marker ends its own capture; it is not a secret.
`output(shellId)` reads the last 64 KiB of regularized text; `retainedFrom` is the absolute
start of that 64 KiB window, not the start of the 1 MiB display capture. A cursor below the
window is reset to `retainedFrom`; `tail` previews it; no other reader touches the PTY. The worker orders the shell terminal: after the process exits it waits for the reader
(bounded by `EXIT_STDIO_GRACE`), the reader calls `finish`, then the shell terminal sweeps
children. The final unterminated line is in the command capture, its log and the display
capture exactly once.
### 3.2 Approved compatibility: event API removal
The user/PR owner approved for this PR: delete `output(eventCursor)`, the `OutputPage`
event fields, `ShellEvent`, `ShellState`, `OutputRequest.waitMs/eventCursor` and
`ackEventCursor`. No in-repo consumer exists; generated TypeScript drops the fields; the
alpha plugin API break is accepted for #1295. Replacement: terminal records plus `wait`,
`output`, `tail`, `retainOutput`; the owner reader is the only lossless consumer. There is no
public fallback queue and no ack cursor. The owner keeps a crate-private line queue only for
`ShellSession`; it is an implementation detail, not a public contract.
## 4. Flows
### 4.1 ShellSession: state and exact owner calls
Stored: `shell: Option<TaskId>`, `generation`, `dialect`, `shell_exe`, `queue`,
`queue_running`, `occupying: Option<{task_id, output_path, delivered}>`,
`background: HashMap<TaskId, {task_id, output_path, cancel}>`, `shell_exit`, `had_reset`.
A `write`/`resize` `task_ended` means the shell is terminal → step 7.

1. **Init.** `shell(...)` → id/path; store, bump generation; spawn the shell watcher
   `wait(shellId, None, lifecycle)` → step 7. `write(shellId, data, start=Command{initId})`;
   one `wait(initId, INIT_TIMEOUT)`; terminal → `settle(initId)`, remove its internal log;
   failure → `kill(shellId)`, `wait`, `settle`, clear shell, fail the first call.
2. **First foreground.** `write(shellId, data, start=Command{commandId})` → id/path;
   register the path. One `wait(commandId, timeout_ms, cancel)` — the same one-wait policy
   as exec's 30 s foreground wait, no poll loop. Terminal → 4; `cancelled` → 5; `running`
   → 3; `task_unknown` → 7.
3. **Occupancy.** Append `exec/started`; on failure `kill`, `wait`, `settle`, clear
   occupying, return the fact failure — no phantom fact. Set occupying; watcher
   `wait(commandId, None)`; terminal → 4, append `exec/ended`, settle, clear, notify.
   Return "still running" with the preview `output(commandId, 0, 16 KiB)`. The next call's
   `occupied_wait` waits on the notify or its deadline; if still occupied it reads
   `output(occ.taskId, delivered, 16 KiB)` and does not execute the new command.
4. **Result.** `output(commandId, 0, 16 KiB)`; exit line from `WaitReply`; omitted =
   `outputBytes - bytes` when `bytes < outputBytes`; `is_error` = status not `exited{0}`;
   settle after the text is materialized.
5. **Interrupt.** `kill(commandId)` (ETX only if active); `wait(commandId, Some(2 s))`;
   terminal → 4 with the interrupted note; still running → kill the shell, wait for child
   watchers, settle; the sweep writes `failed{shell_reset}`.
6. **Background.** `write(shellId, data, start=Job{...})`; register; append `exec/started`
   (on failure kill/settle, no fact); watcher `wait(jobId, None)`; terminal → result,
   append `exec/ended`, settle, remove. A stop kills and waits; `task_running` keeps the
   watcher and returns a stop-requested result; a kill error keeps the watcher too; the
   owner terminal fact follows.
7. **Shell exit.** The watcher gets the terminal; ignore an older generation. Set
   `shell=None`, `had_reset=true`, `shell_exit=note(status)`; the sweep has terminaled the
   children and their watchers write facts. Wait for child watchers (separate tracker),
   then `settle(shellId)`; the next call respawns and prepends the note.
8. **Reads.** `session.exec.output` → `tail(hostTaskId)`; `outputPage` → `output(hostTaskId,
   cursor, limit)`; a gone record reads the registered file; the shell display read is
   `output(shellId, ...)`/`tail(shellId)`. A `wait` timeout is `running`, no kill; settle
   only terminal; a repeated settle is `Ok`.
### 4.2 Exec: start, wait, handoff, stop
```text
exec: read execShell() for the edition; keep & and heavy-search refusals
spawn → {taskId, outputPath}; wait(taskId, 30s, cancel)
  terminal → result → settle; cancelled → kill/wait → result;
  running → retainOutput → BackgroundExec.start(hostTaskId, taskId, path) → watcher
  (watcher: stop → kill/wait/exec-ended; terminal → result/exec-ended/settle)
```

One result builder: `text` → inline content + note; file → `Output saved to:`, bytes,
truncation line, note, `preview`; `failure` → its code/message (`OutputWriteFailed` keeps
partial output); `status` → `completed` only for `exited{0}` without failure. Preserved:
inline below 30,000 UTF-16 units is complete (including `seq 1 5000`); above it the full
file; offload failure keeps partial output; `StartRequest` carries `task_id` (branch,
hook-rewritable) and `host_task_id`. Facts: `WorkspaceTools`/`BackgroundExec` and
`ShellSession` append `exec/started` after the owner accepted the start (on failure the task
is killed/settled, no phantom start) and `exec/ended` after the terminal and before settle;
the owner writes none; recovery reads them as today and never restarts a process.
## 5. Boundaries
### 5.1 Paths, logs, registration
Data-face paths are UTF-8 `String`, absolute unless stated; no `to_string_lossy` on a
request or reply. A non-UTF-8 owner path makes `info`/`execRoot` answer `invalid`; a
non-UTF-8 shell path is `exec_shell_unavailable`. In-process producers use `to_str()` and
fail their own call; SessionStore removes a non-UTF-8 candidate with FileStorage. Log is
`{outputDir}/{taskId}.log`; files are created 0600 in the output directory the caller provides. Exec log is lazy at the inline threshold or
`retainOutput`; `symlink_metadata` rejects any entry (dangling included) as `task_exists`.
Shell command/shell/job logs are `create_new`; a failed PTY start removes the log; a job
completion file is checked for absence. `retainOutput` is idempotent: exec creates the file
and switches to file mode; shell marks the PTY log retained until `removeOutputDirectory`.
Registration is the branch's: `ToolDriver` registers an `outputPath` only when the model
result carries it and the file exists; it never registers a planned exec path. `ShellSession`
registers the shell `outputPath` before the `shell` reply leaves and the `write` reply path
after start. Command logs are registered; the init log is internal and is not.
### 5.2 Markers, pid, state
Shape/validation/scope: §3.1; `write` rejects a malformed/empty marker with `invalid`. The
pid line is `__DIM_END_...__<pid> <taskId>`, scoped to its shell and a registered id; a job
without a pid cannot be signaled immediately: 2 s, `task_running`, then a late pid is
killed and released before the terminal. `ShellState`/`ready` is deleted: readiness is the init terminal.
### 5.3 MCP child PATH: final rule
```rust
// native, in-process, not served
fn child_path_process(&self, directories: &[String], base: &OsStr) -> Option<OsString>
// None = nothing to append; Some = composed. An OsStr base is ordinary data.
```
| Case | Native | Served `childPath` | MCP caller |
| --- | --- | --- | --- |
| no platform / no addition | n/a / `None` | n/a / `None` | `None`; leave PATH |
| composition | `Some(OsString)` | `Some(String)` | set PATH |
| non-UTF-8 daemon base | accepted as `OsStr` | not expressible | native binding; no error |

A non-UTF-8 daemon PATH is not an error: on Unix it is legal data and receives the runtime
directories; on Windows `OsString` is UTF-16. The served face cannot receive one, so no
silent loss remains; this is the same owner operation, not a second implementation.
### 5.4 `removeOutputDirectory` and docs
The request path is UTF-8. Reject `.`/`..` and any symlink component between canonical
`execRoot` and the target (`invalid_path`); canonicalize the target when it exists, or its
parent when absent, and require a strict descendant of the canonical root (equal is
`invalid_path`); remove; absent is `Ok`; other I/O is `io`. A path outside `execRoot` is
`invalid_path` (SessionStore falls back to FileStorage).

`resource-faces.md` §3 owns the contract; `HostPlatform.md` owns the shape and links;
`WorkspaceTools.md` owns exec/BackgroundExec and links; `service-faces.md` §3.4 lists
operations and attributes the PTY to `ShellSession`; npm declarations regenerate.
## 6. Verification
| Case | Evidence |
| --- | --- |
| shell command page, UTF-8 cut, retained log, second command, kill | `host_platform_flow::a_shell_command_capture_is_owned_by_the_host` |
| shell terminal, `TaskEnded`, idempotent settle | `host_platform_flow::the_host_platform_service_serves_a_shell` |
| public shell text burst without an event cursor | `host_platform_flow::a_shell_burst_completes_without_a_private_subscriber` |
| shell exit sweeps commands/jobs; stop after a pid | `minimal_execution::shell_exit_settles_tasks`, `shell_exit_kills_background_jobs`, `stop_background_task` |
| inline exec behavior | `local_execution::foreground_output_is_canonical_and_offloaded_like_ts` |
| page accounting and restored tasks | `transport_flow/shell_input::background_exec_output_pages_running_finished_and_restored_tasks` |
| Windows confined outcomes | `host_platform_windows_flow` |
| docs/types | `xtask check`, `types`, `graph` |

Existing flows stay (`host_platform_flow`, `local_execution`, `minimal_execution`,
`transport_flow/shell_input`). Full commands and counts are in the PR body, so this table
does not claim a test that is not in the tree.
## 7. Migration
### 7.1 Buildable steps

| # | Parent | Commit | Files | Compile boundary | Gate |
| --- | --- | --- | --- | --- | --- |
| 1 | `984f6b59d1` | `refactor(host-platform): one task record and per-kind payload` | `host_platform.rs` (+ private `task`), new `output.rs`, `exec.rs` (capture/command move), `shell_session.rs` (payloads, no policy), `shell_input.rs` imports, `HostPlatform.md` | `TaskRecord`/`TaskKind` for all four; `ExecProcess`, `ExecutionTask`, event fields and public shapes still exist; `ShellSession` still reads `output(eventCursor)`. If the four payload moves do not compile together, split 1a/1b; do not claim one commit. | `host_platform_flow`, `local_execution` foreground/background, `minimal_execution` shell, clippy |
| 2 | step 1 | `refactor(host-platform): one snapshot, typed errors and failures` | `host_platform.rs`, `exec.rs`, `shell_session.rs`, `background.rs`, `hooks/runner.rs`, `modality/engine.rs`, npm, docs | §2.2–2.4; `ExecProcess` still exists and reads the snapshot; event fields still exist; all matches updated; `TaskFailure` replaces `outputFailure`; `retainOutput` switches a Running capture to file mode and only side-copies a Terminal one (never mutates a terminal snapshot) | `host_platform_flow`, `local_execution`, `minimal_execution`, `hooks_flow`, `ocr_helper_flow`, `mcp_session_flow`, `process_typescript` |
| 3 | step 2 | `refactor(exec): run commands as owner task ids` | `exec.rs`, `background.rs`, `workspace_tools/mod.rs`, `local_execution.rs`, `WorkspaceTools.md` | `ExecProcess`, `CommandTask` and `ExecutionTask` deleted; one `TaskRecord` table; `BackgroundExec` holds ids, live reads via `tail`/`output` | `local_execution` handoff/stop/recovery, `transport_flow/shell_input` |
| 4 | step 3 | `refactor(host-platform): splitter, shell settlement, event API removal` | `host_platform.rs`, `shell_session.rs`, `minimal_execution.rs`, `npm/*`, `HostPlatform.md`, `resource-faces.md` | Owner splitter; markers/pwsh/sweep; `write`/`resize` on a dead shell answer `task_ended`; `ShellSession` has no marker scan/retained/event loop; event API deleted | `minimal_execution` shell, `host_platform_flow` burst/display + dead-shell write, `xtask types` |
| 5 | step 4 | `fix(host-platform): one boundary policy` | `host_platform.rs`, `exec.rs`, `shell_session.rs`, `stdio.rs`, `storage/files.rs`, `hooks/runner.rs`, `modality/engine.rs`, flow tests | UTF-8 paths; log reservation/cleanup/retention; strict removal; typed MCP PATH | `host_platform_flow`, `host_platform_windows_flow`, `mcp_session_flow`, `ocr_helper_flow` |
| 6 | step 5 | `fix(app-server): route session.exec.stop through the branch stop owner` | `connection.rs`, `transport_flow/shell_input.rs`, `minimal_execution.rs` | Minimal's `ShellSession` already provides `dyn ExecStop`; `session.exec.stop` uses it when present, otherwise the local profile keeps `BackgroundExec`. `BackgroundExec` gains no `ShellSession` dependency; a future profile with both task kinds must provide one stop dispatcher in its assembly. | live shell command/job stop through `session.exec.stop`; normal exec stop unchanged; `product_protocol` |
| 7 | step 6 | `docs(host-platform): one owner contract` | `resource-faces.md`, `HostPlatform.md`, `WorkspaceTools.md`, `service-faces.md`, `resource-owner-operations.md`, `ShellInput.md`, `npm/plugin.d.ts`, generated | One rule per page; identical operation lists; PTY attribution; registration split | `xtask check`, `graph`, `types`; `node --test scripts/*.test.mjs` |

### 7.2 Patch mapping
| Change | Keep | Rewrite |
| --- | --- | --- |
| Job reads, acks, exec ids | jobs readable by id; retention explicit; exec uses `spawn`/`wait`/`kill`/`settle` | job records in the one table; `wait`/`output`/`tail`/`retainOutput`; terminal record replaces `ExecProcess`; `hostTaskId`; no rebuilt `ExitStatus` |
| Stream | no marker may be lost; display never backpressures | owner is the only line consumer; queue/cursor/ack deleted; one text capture is the display read |

### 7.3 Scope and follow-up
`task_ended` is in this PR: step 2 adds it, step 4 makes `write`/`resize` return it, and
`write_to_a_terminated_shell_answers_task_ended` gates it; no `connection.rs` change for
that case. `session.exec.stop` is step 6: the minimal `ShellSession` `ExecStop` provider
serves shell tasks; the local profile keeps `BackgroundExec`. `BackgroundExec` gains no
`ShellSession` dependency; `ShellSession`'s existing recovery dependency on `BackgroundExec`
stays.
## 8. Rule check
| Rule | Application |
| --- | --- |
| §1 dependence / ownership; ADR 002 D1 | HostPlatform owns processes/PTYs/tasks; ShellSession policy; WorkspaceTools semantics; BackgroundExec facts. No new capability. |
| Timeline facts; rules 1–4, 9 | No fact added/moved; producers unchanged; owner writes none. |
| One owner/record/watch/splitter/page/error enum | Four records → one with per-kind payload, three notifiers → one watch, two page types → one, folded messages → one `HostPlatformError` and one `TaskFailure`. |
| Data face, no live handle, no duplicate owner | Public methods are serde data; native handles stay native; exec/BackgroundExec hold ids and paths; the owner is the only marker interpreter and terminal writer. `session.exec.stop` selects the branch stop provider; `BackgroundExec` gains no `ShellSession` dependency. |
| Rule 3 / rule 7 / test rule / ADR 002 D4 | Reader is a blocking thread, supervisors/waiters are tasks, terminal transition never awaits under a lock; reuses `pump`/`Decoder`/`bounded_preview`, `sessionFiles`, `Timeline`, `SessionLifecycle`; boundary tests are face flows; switches read per prepare, shell preference per spawn. |

Exceptions: `kill` on a shell command requests an interrupt and the marker or sweep decides
the terminal (the stop producer still writes `cancelled`); `wait` timeout is a read;
`child_path_process` takes `OsStr` in-process.

Ablation: the added fields are values consumers were reconstructing (inline text, failure
kind, duration, exit/signal, started path), the page's `totalBytes`/`retainedFrom`/`reset`,
and `task_ended`; each has a consumer and a test.
