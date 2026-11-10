# Resource leaf data faces

Status: Draft. Issues: [#1295](https://github.com/arcships/dim/issues/1295) (`execHost`),
[#1296](https://github.com/arcships/dim/issues/1296) (leaf identity and lifecycle),
[#1297](https://github.com/arcships/dim/issues/1297) (`fsHost`); parent
[#1292](https://github.com/arcships/dim/issues/1292). Policy: [service faces](service-faces.md).
Background: [remote plugins §3–§4](research/remote-plugins.md).

This is a design, not an implementation record. Every operation below is a target shape.

## 1. Scope

A resource leaf owns the machine's files, processes, PTY and output files. It provides two data
faces:

| Face | Operations | Issue |
| --- | --- | --- |
| `execHost` | `start`, `wait`, `kill`, `output`, `shell`, `write`, `resize` | #1295 |
| `fsHost` | `facts`, `read`, `write`, `edit`, `glob`, `grep`, `mentions` | #1297 |

```text
 control node                                        resource leaf (mounted per #1296)
   WorkspaceTools          ── fsHost ─── data ──────►  file operations on node files
   exec, ShellSession      ── execHost ─ data ──────►  processes, PTY, output files
   Permission and Approval                            never asked, never decides
   Timeline facts, task records, sessionFiles rows
```

Both faces are mounted by a leaf, so a name resolves per leaf. How a leaf id selects the
instance, how the leaf dials the host, the token or certificate, reconnects and the probe are
[#1296](https://github.com/arcships/dim/issues/1296)'s contract; this document does not restate
them. It fixes only the operation shapes, the data they carry and the ownership of what they
return.

What stays on the control side, in every case: Session and branch scope, admission
(`Approval`, and `Permission` with the [ADR 014](../adr/014-file-access-facts.md) table), the
`exec/started` and `exec/ended` facts and the wake notice, the task record, Session-file rows,
and the recorded tool result. The leaf never asks the user anything and never decides whether a
call may run.

## 2. Rules for these two faces

1. **One call per operation.** `read`, `write`, `edit`, `glob`, `grep` and `mentions` are one
   call each; a traversal is one call, not one call per file.
2. **Paths are node paths.** A request carries a node-absolute resolved path; a reply returns
   node-absolute resolved paths only. A relative path in a request is `invalid`. Returned paths
   are stable for history: the same file always resolves to the same path.
3. **Ids are values.** A task id is a string the caller assigns; ids carry no authority.
4. **Pages are bounded.** Large output and long results cross as pages with a cursor, never as a
   stream. No reply carries unbounded bytes.
5. **Outcomes and failures are different data.** What the operation did is a tool result
   (`content`, `structuredContent`, `isError`, `effects`) and keeps the codes it uses today. A
   call that could not be served is `{code, message}`; the control side returns that code and
   message as the tool result unchanged, so the two vocabularies stay one.
6. **Cancellation is a call.** The caller drops the call or sends the bridge's cancel. Cancelling
   a read of an outcome changes nothing; cancelling a process is `kill`.
7. **No secret, no approval, no prompt.** Neither face carries a credential, a preset or a user
   decision.
8. **Names are camelCase.** Request, reply and error members are camelCase, as the rest of the
   migration's wire types are.

## 3. `execHost`

### 3.1 Methods

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `start` | async | `{taskId, command, cwd, workspace}` | `{outputPath}` |
| `wait` | async | `{taskId, timeoutMs?}` | task record (§3.2) |
| `kill` | async | `{taskId}` | `{}` |
| `output` | async | `{taskId, cursor?, limit?}` | page (§3.4) |
| `shell` | async | `{taskId, cwd, workspace, cols, rows}` | `{outputPath}` |
| `write` | async | `{taskId, data}` | `{}` |
| `resize` | async | `{taskId, cols, rows}` | `{}` |

### 3.2 `start` and `wait`

`start` accepts only `{taskId, command, cwd, workspace}`. `cwd` is the model's requested working
directory and `workspace` the Session's workspace root, both passed as the caller holds them; an
absent `cwd` is `workspace`. The node resolves `cwd` against `workspace` and a path that does not
exist is `cwd_not_found`. The node builds the shell, argv and environment from its own current
shell preference, prepares the boundary from its own switches, spawns, and creates the task's
output file. `taskId` is assigned by the caller (the control side allocates `exec-<uuid>` as it
does today); a second `start` with a live id is `task_exists` and starts nothing, so a retried
call after a lost reply cannot run the command twice.

`start` has no boundary input. A request that still carries `sandbox`, `network` or any other
process-boundary field is `invalid`: the switch is not the caller's to send
([ADR 003 D1](../adr/003-approval-scope.md), [process isolation](host-platform-isolation.md)).

`wait` returns the task record once the process tree has ended:

```text
{status: "exited" | "signaled" | "timed_out" | "cancelled" | "failed",
 exitCode?: number, signal?: string, durationMs: number,
 outputPath: string, outputBytes: number, outputTruncated: bool,
 preview: string, previewTruncated: bool,
 failureNote?: string}
```

- `preview` is the bounded sanitized tail the exec rules already define (the last 2,000 lines
  within 50 KiB); `outputPath` is the append-only file on the node; `outputTruncated` is the
  64 MiB cap.
- `failureNote` is the isolation owner's note, present only when a sandboxed command failed and
  its shown output matches a typical denial. Producing it is the node's; appending it to the
  result text is the control side's.
- `timeoutMs` bounds the call's wait, not the process's obligation: reaching it kills the tree and
  answers `status: "timed_out"`. The control side computes the effective timeout from its own
  policy and passes it; a `wait` cancelled by the caller ends only the call, and the caller sends
  `kill` if it wants the process gone.
- `wait` is repeatable: a terminal record stays readable until the leaf settles the task. Without
  `timeoutMs` the call waits for the end; the control side passes one whenever its policy has one.

### 3.3 `kill`

`kill {taskId}` terminates the task's process tree with the existing tree-kill and answers `{}`.
An ended task is a no-op that still answers `{}`; an id the leaf never issued is `task_unknown`
and kills nothing. `exec_stop` stays a control-side tool: it calls `kill` and then `wait`, and
words its own `{task_id, status, result}`.

### 3.4 `output`

`output {taskId, cursor?, limit?}` reads the task's output file by byte cursor and answers
`{text, bytes, nextCursor?, truncated}`. A missing `cursor` is the file's start. `limit` rejects
`0` (`invalid`), defaults to 1 MiB and clamps to 4 MiB. `text` is sanitized exactly like the
preview; `bytes` is the file bytes this page consumed; `nextCursor` is present only when the page
was full and equals `cursor + bytes`; `bytes == 0` means caught up, so the caller retries the same
cursor. The file is append-only, so a cursor the node handed out always names a byte that never
changes, and a page never splits a character. These are the rules of
[background command output](background-task-output.md); this face adds no second page rule. The
reply never selects a path: the task id does.

### 3.5 Persistent shell

`shell {taskId, cwd, workspace, cols, rows}` creates one persistent PTY shell as a task;
`write {taskId, data}` sends `data` (base64 bytes) to it, `resize` sets the geometry, `output`
reads its captured text, and `kill` ends it. The shell's `read` is `output`: one page operation
with one cursor rule serves both a command's output file and the shell's capture, and a second
name for the same operation would be a duplicate. The capture is bounded (64 KiB today), so a
cursor before the retained start is `cursor_expired` and the caller continues from the earliest
retained position.

The command protocol that runs on top of the shell — marker pairs, occupancy, the queue and the
foreground timeout — is not part of this face. Where it runs is an open decision (§9); if it stays
on the control side it uses `write`/`resize`/`output` and pages, never streams.

### 3.6 Ownership, disconnect and cancellation

- The leaf owns the process, the PTY, the temporary directory and the output file. The branch owns
  the task record and the facts: `exec/started` after `start` returns the id and output path,
  `exec/ended` when the terminal record is in, and the wake notice. A background handoff moves the
  control-side task record only; the process never moves.
- When the link drops, the node's own `HostPlatform` ends the processes it started
  ([overview §6.3](overview.md#63-multiple-nodes)); the leaf's task table does not survive the
  connection, so every task id is settled there. A reconnect provides the services again and
  reloads nothing: the Session keeps running.
- A control-side task that was waiting on a dropped link settles once, as failed with
  `node_unavailable`, and is never restarted; the command may have run. A `start` whose reply was
  lost is not retried with the same id.
- Cancelling an `exec` call kills the task before the call returns; a cancelled `wait` only ends
  the call. A leaf closing, or a leaf that unloads the face, kills its tasks before it stops
  serving; a node whose App Server dies leaves only the backend's parent-death handling.
- #1296 fixes the leaf's lifecycle and states the disconnect rule for owned processes. This
  document depends on that rule; it does not define reconnect or mount semantics.

### 3.7 Settings and the boundary

The node reads its own `sandbox` and `network` switches through its `ExecutionSettings` part at
every `start` and `shell`, uses its own exec-shell preference and captured `PATH`, and prepares
the process with its own platform roots and temporary directory. A running task keeps the boundary
it started with. A missing or unreadable switch source is `isolation_config_invalid`; sandbox on
without a backend is `isolation_unavailable`; both refuse the payload before it runs, exactly as
[process isolation](host-platform-isolation.md#failure) states, and nothing falls back to a weaker
boundary.

The control side sends no switch, reads none, and cannot widen a boundary through `cwd`,
`workspace` or `command`. Its tool declaration line stays what it is; it describes the switches,
not their value. Node-local surfaces — the exec-shell preference, the backend availability report,
`home` and `execRoot` — are not operations of this face: they are the node's own management, shown
by whoever manages that node.

Host-owned helpers (`spawn_confined`) are not an operation of this face either: they run for a
plugin's own purpose and are not agent-decided commands.

### 3.8 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The leaf is not connected. Only this call fails |
| `invalid` | A malformed request: relative `cwd`, `limit: 0`, a boundary field |
| `task_exists` | `start` or `shell` with a live task id |
| `task_unknown` | An id this leaf's connection never issued |
| `cwd_not_found` | The resolved `cwd` does not exist |
| `exec_shell_unavailable` | The node's shell preference cannot be resolved (with its own reason) |
| `isolation_unavailable` | The sandbox is on and the node has no backend |
| `isolation_config_invalid` | The node's switches cannot be read |
| `spawn_failed` | The process could not start |
| `output_unavailable` | The task's output file is gone |
| `cursor_expired` | The cursor precedes the retained shell capture |
| `cancelled` | The call was cancelled before it changed anything |

## 4. `fsHost`

### 4.1 Methods

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `facts` | async | `{path, cwd, workspace}` | path facts (§4.2) |
| `read` | async | `{path, offset?, limit?, encoding?}` | `{result, binding?}` (§4.3) |
| `write` | async | `{path, content, binding?}` | `{result, binding?}` |
| `edit` | async | `{path, oldText, newText, replaceAll?, binding?}` | `{result, binding?}` |
| `glob` | async | `{pattern, path, limit?}` | `{result: ToolResult}` |
| `grep` | async | `{pattern, path, include?, outputMode?, lineNumbers?, limit?, multiline?, literal?, timeoutMs?}` | `{result: ToolResult}` |
| `mentions` | async | `{path, query, limit?}` | `{candidates}` |

Every operation works on node files. Session files and exec output files are ordinary node files
to these operations; the rows that describe them stay on the control side (§4.6).

### 4.2 `facts`

`facts {path, cwd, workspace}` resolves `path` the way the file tools do — `cwd`, `~`, `..` and
symlinks settled, and the longest existing prefix resolved so a Session started through a
symlinked directory still matches — and answers:

```text
{path: string,            // the resolved node path the caller then uses
 workspace: boolean,      // inside the workspace, or a confirmed worktree root
 system: boolean,         // inside the node's platform roots
 discoveryError?: string} // worktree recognition failed; the workspace was not expanded
```

`workspace` covers the [worktree access](worktree-access.md) roots the node recognizes: discovery
uses the node's repository, and a failure is reported as `discoveryError` without expanding
anything, as it is today.

The other two facts of [ADR 014](../adr/014-file-access-facts.md) are Session records, not node
knowledge: `owned` is `sessionFiles.owns {sessionId, path}` ([service faces §4.5](service-faces.md#45-sessionfiles))
and `grant` is a comparison against the Session's grant rows. Both are answered on the control
side from the node's resolved `path`; neither reads a filesystem, so neither needs the node. The
control side then applies ADR 014's table with the branch preset, and `Permission` and `Approval`
stay exactly where they are.

### 4.3 `read`, `write` and `edit`

The reply is the operation's own tool result, plus the file binding when the operation established
or changed one:

```text
{result: ToolResult,            // content, structuredContent, isError, effects
 binding?: {rawRevision: string, // SHA-256 of the raw bytes, lowercase hex
            encoding: string, encodingSource: string, bom: string, eol: string}}
```

- `read` keeps its line numbers, its `offset`/`limit`, its encoding, its image blocks and image
  scaling, and its refusals (`file_not_found`, `is_directory`, `binary_unsupported`,
  `offset_out_of_range`, `read_error`, `permission_denied`, `symlink_loop`) because the operation
  that produces them runs on the node. It returns a `binding` for a text file and none for an
  image.
- `write` and `edit` keep their refusals (`text_binding_required`, `file_modified_since_read`,
  `is_directory`, `write_error`, `edit_target_file_too_large`) and their `effects: [fileWritten]`
  declaration; the binding the control side holds from the branch's last read of that file travels
  with the request, so the node normalizes and encodes exactly what the model's context was based
  on. A file that changed since the read is `file_modified_since_read`. The reply's `binding` is
  what the call installed.
- The control side records the returned binding in `FileReads` for the branch; that record stays
  branch memory on the control node and is not reconstructed from the node.
- `write` and `edit` are admitted before they run, so their `path` is the resolved path `facts`
  returned. `ToolHost.resolve_write` asks `fsHost.facts` for a remote Session instead of resolving
  locally; the resolved call is what approval and execution receive, as today.

### 4.4 `glob`, `grep` and `mentions`

- `glob {pattern, path, limit?}` walks the node's tree once and returns the bounded preview it
  returns today (at most 100 records, `limit` 1–100).
- `grep {pattern, path, include?, outputMode?, lineNumbers?, limit?, multiline?, literal?,
  timeoutMs?}` searches the node's tree once, with the existing per-file match cap, preview byte
  cap, deadline and smart-case rules.
- `mentions {path, query, limit?}` is the `session.files.search` operation on the node: it answers
  `{candidates: [{relativePath, absolutePath, dir}]}` from the node's workspace walk with the
  existing ranking, `limit` default 20 and clamp 1–50. The index behind it is node-internal;
  `mentionIndex` stays a host-resource name on the node side and is not a face.

All three take the resolved search root in `path`; the control side resolves and admits that one
path before the call, so admission is still one decision per call and the traversal happens once,
on the node.

### 4.5 Paths, facts and approvals

A path outside the facts is refused by the same rule as today, evaluated on the node's filesystem:
the `facts` call resolves and classifies, the control side's `Permission` decides with the branch
preset, and `Approval` asks the user when the table says to ask. No approval, preset, grant or
retry is sent to the node, and the node has no policy of its own.

Returned paths are the node's resolved absolute paths, quoted in history and stable for the file's
life. A read of a Session-owned artifact still passes file scope through the `owned` fact without a
grant, and tool approval still follows the preset
([session files §11.1](appserver/session-files.md#111-reading-a-session-file)).

### 4.6 Session files and exec output

Bytes stay on the node. `sessionFiles.put` already writes into the resource node's `FileStorage`
and returns a node path ([session files §3.1](appserver/session-files.md#31-on-disk)); this face
reads and writes those files like any other node file, and `sessionFiles` remains the owner of the
rows. An exec task's output file is the node's, and `execHost.output` is how it is paged. Neither
gets an `fsHost` operation of its own, and neither moves bytes through the control side.

### 4.7 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The leaf is not connected. Only this call fails |
| `invalid` | A relative path, an unbounded page, a malformed request |
| `cancelled` | The call was cancelled before it changed anything |

Operation refusals are not in this table: they arrive as the tool result's own `isError` with the
code the operation already uses, so one code vocabulary serves the local and the remote path.

## 5. The store is not a leaf face

`sqliteStorage` and `timelineStore` do not become data faces here, and this design adds no store
face such as a `dbHost`:

- `TimelineStore` is the port under a branch's `Timeline`. The before chain, sequence allocation,
  the publish watermark and the append-time publish callback are branch-scope ordering
  ([timeline storage](../architecture/rust-agent/timeline-storage.md) §1, §3). Crossing them makes
  a commit wait on a link, and a dropped link stops the Session rather than failing one call — the
  opposite of the resource seams' failure unit.
- `SessionStore` is more than the Timeline: rows for Sessions, owners, grants, titles, settings and
  Session files, plus the fork and delete transactions. Moving one table across would split the
  state that commits atomically today.
- Remote state is [#1298](https://github.com/arcships/dim/issues/1298): a PostgreSQL backend for the
  same repository operations, in the same process, with no dual write and no SQLite fallback.

So the shape these names need is not an operation face. The pull request that implements #1298
states its own storage contract; this document does not pre-empt it, and nothing here requires the
storage plugin to be mounted on a link. Both names stay `internal(object)` while their connection
and batch are in-process state: #1298 changes which engine answers them, not whether they cross.

## 6. Not in these faces

`hostPlatform` and `fileStorage` themselves stay `internal(object)`. A leaf cannot provide or
inject a handle, and both are handle-shaped inside the node: a process tree, a PTY, a leased
root-relative file operation. What crosses is the operation face above, provided by the leaf as a
new name and implemented with the node's own `HostPlatform` and `FileStorage`. Their internal
names are not migration debt left behind; the data face exists under its own name.

| Thing | Why not |
| --- | --- |
| `fileStorage` | The node's own trusted storage mechanism: roots, leases, one validated component per open. `fsHost` and Session files use it inside the node; it is not a name leafs call |
| `mentionIndex` | The node's walked index behind `mentions` |
| `sandboxBackend`, exec-shell settings, `home`, `execRoot` | Node-local management and facts, not agent operations |
| `spawn_confined` | A host-owned helper for a plugin's own purpose, not an agent-decided command |
| Session-file rows, grants, titles | Control-side records (§4.2, §4.6) |
| Approval and Permission | Control side, unchanged |

Runtime handles are not durable identities: a task id or a resolved path is a value in a call, not
a claim on a resource, and none of them confers authority
([service faces §1](service-faces.md#1-规则)).

## 7. Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | The faces belong to the leaf that owns the resources; the Session and branch parts that consume them stay on the control node. No new Capability on the control side |
| §4 rule 1 | Command events are not added. A background command's fact is still `exec/started`/`exec/ended` on its branch, written by its owner |
| §4 rule 2 | A handoff keeps its fact on the branch and its process on the leaf; nothing is handed to a second owner |
| §4 rule 3 | No before callback and no commit-time I/O: no face operation runs inside the commit lock |
| §4 rule 4 | Core, assembly and the Timeline carry no remote-execution branch; the leaf is mounted, not special-cased |
| §4 rule 5 | No batch semantics change: no operation writes events |
| §4 rule 6 | File content and command output reach the model only through tool results, as they do today |
| §4 rule 7 | Consumers use the faces; they read no leaf internals. The leaf uses its own HostPlatform and FileStorage |
| §4 rule 8 | Existing mechanisms suffice for the faces. The host side's per-call service resolution is [#861](https://github.com/arcships/dim/issues/861), already raised by [overview §6.3](overview.md#63-multiple-nodes); this document raises nothing new |
| §4 rule 9 | No new Timeline fact: `exec/started` and `exec/ended` already exist, and the faces write none |
| ADR 002 D2 | No wiring plugin: each face is the leaf's own service |
| ADR 002 D3 | Dependencies follow scope: a leaf provides process-scope names; consumers resolve per call |
| ADR 002 D4 | Business configuration is read at use time on the node: the switches at each `start`, the shell preference at each spawn |
| ADR 002 D5 | §3 and §4 are the language-neutral contract; the Rust and TypeScript bindings implement them |
| ADR 003 D1, D5 | No boundary input in a call; a preset, approval or replay confers none |
| ADR 009 | Permission decides, the node's `HostPlatform` enforces |
| ADR 010, ADR 011 | The switches stay the node's user-scope settings, read and enforced there |
| ADR 012, ADR 013 | Grants and Session-file ownership stay control-side records; paths are the node's |
| ADR 014 | The four facts keep their sources; resolution and the workspace and system facts are the node's, the row facts the control node's |

## 8. Requirements raised to mechanism owners

None beyond what is already raised. [#861](https://github.com/arcships/dim/issues/861) is the one
dependency: the host side resolves a Session's process-scope service per call, by the Session's
resource node, which [overview §6.3](overview.md#63-multiple-nodes) already states as a
requirement on the runtime and on rutis. Neither face needs a new event, a batch change, a
synchronous callback across the link, or a Timeline rule.

## 9. Open decisions

1. **Naming and instance per leaf.** Process-scope names are global, so `execHost` and `fsHost`
   need one instance per leaf: `execHost.<leaf>` or a host-side route. [#1296](https://github.com/arcships/dim/issues/1296)
   decides; this document assumes only that a Session's calls reach one leaf.
2. **Disconnect policy.** The node ends the processes it started, per overview §6.3. #1296 states
   and tests the rule for mounted resource leaves; the control-side settlement (`node_unavailable`,
   no restart) follows from it.
3. **`owned` on the control side.** The fact is a row lookup, so it is answered by `sessionFiles`
   with the node's resolved path rather than on the node. If review wants it computed with the
   other three, the rows would have to move, which is #1298's kind of decision.
4. **Where the persistent-shell protocol runs.** `write`/`resize`/`output` are enough for a
   control-side marker and occupancy protocol that pages its output; running the protocol on the
   node would make it `start`/`wait` instead. #1295 decides when it wires PTY, after foreground and
   background.
5. **Result text ownership.** File operations return their tool result from the node because the
   text is the file operation's own (line numbers, previews, image blocks). A command returns
   records, and the control side composes the tool result, because the task identity, the
   background policy and the output-path registration are its. If a migration agent finds the
   split costs more than it saves, the alternative is a command tool result built on the node plus
   a control-side registration step — but only one of the two is chosen, not both.

## 10. Ablation

The faces are operations over data with ids, not adapters: `start`/`wait`/`kill`/`output` reuse the
existing process tree, capture, page rule and fault vocabulary; `shell`/`write`/`resize` are the
PTY primitives the shell already has. `fsHost` adds no primitive file API, no per-file traversal,
and no policy of its own: admission, approvals and record keeping stay where they are, and the
`facts` call is needed because a path's facts cannot be computed on a machine that does not hold
the file. The store is deliberately left out rather than given a face that would have to carry a
write round across a link. Nothing else was added that a caller could not do without.
