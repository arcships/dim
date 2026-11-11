# Resource and store data faces

Status: Draft. Issues: [#1295](https://github.com/arcships/dim/issues/1295) (`execHost`),
[#1296](https://github.com/arcships/dim/issues/1296) (leaf identity and lifecycle),
[#1297](https://github.com/arcships/dim/issues/1297) (`fsHost`),
[#1298](https://github.com/arcships/dim/issues/1298) (`store`); parent
[#1292](https://github.com/arcships/dim/issues/1292). Policy: [service faces](service-faces.md).
Background: [remote plugins §3–§4](research/remote-plugins.md).

This is a design, not an implementation record. Every operation below is a target shape.

## 1. Scope

Three faces, each a name whose public contract is data:

| Face | Operations | Provided by | Issue |
| --- | --- | --- | --- |
| `execHost` | `start`, `wait`, `kill`, `output`, `shell`, `write`, `resize`, `info`, `confined` | the process owner | #1295 |
| `fsHost` | `resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions`, `createDirectory`, `replace`, `list`, `remove` | the file-operations owner | #1297 |
| `store` | Timeline and Session-row repository operations | the state owner | #1298 |

```text
 control node                                        resource leaf (mounted per #1296)
   WorkspaceTools          ── fsHost ─── data ──────►  file operations on node files
   exec, ShellSession      ── execHost ─ data ──────►  processes, PTY, output files
   Permission and Approval                            never asked, never decides
   Timeline facts, task records, Session-file rows
   Session state owner     ── store ──── data ──────►  the same face, answered locally
```

**A face is a shape; the mount is a deployment.** `execHost` and `fsHost` are provided by the
daemon that holds the process or the file — the control node's own plugins, or a resource leaf
mounted per [#1296](https://github.com/arcships/dim/issues/1296). `store` is provided where the
Session state is, always in the daemon that owns that Session
([#1298](https://github.com/arcships/dim/issues/1298)); it is the same face whether the rows live in
SQLite or PostgreSQL. Whether a face is reachable over a link is decided by deployment, not by a
classification on the name.

No name here carries an `internal` exception. A shape that used to be an object is reshaped into
operations, a credential is a capability an operation uses and never returns, and a hot path is a
property of the consumer, not a reason to publish a second API. The old names — `hostPlatform`,
`fileStorage`, `sqliteStorage`, `timelineStore` — are the providers and their Rust bindings; they
are not public contracts of their own (§6).

What stays on the control side, in every case: Session and branch scope, admission
(`Approval`, and `Permission` with the [ADR 014](../adr/014-file-access-facts.md) table), the
`exec/started` and `exec/ended` facts and the wake notice, the task record, Session-file rows,
and the recorded tool result. No face asks the user anything, decides whether a call may run, or
consults who is calling.

## 2. Rules for these faces

1. **One face per service.** The operations are the public contract; a Rust typed client is a
   binding over the same operations and owns no second business logic. There is no owner trait
   beside the face.
2. **One call per operation.** `read`, `write`, `edit`, `glob`, `grep` and `mentions` are one call
   each; a traversal is one call, not one call per file.
3. **Paths are node paths.** A request carries a node-absolute resolved path; a reply returns
   node-absolute resolved paths only. A relative path in a request is `invalid`, with one
   exception: **`fsHost.resolve` is the resolution entry point**, so it accepts the path as the
   model gave it — relative to the `cwd` the caller passes, starting with `~`, containing `..`,
   following symlinks — because only the node can resolve it. `cwd`, `workspace` and the
   confirmed roots it is given are node-absolute. Every other operation, `execHost.start` and
   `shell` included, takes a path that a `resolve` reply already returned. Returned paths are
   stable for history: the same file always resolves to the same path.
4. **Ids are values.** A task id is a string the caller assigns; ids carry no authority.
5. **Pages are bounded.** Large output and long results cross as pages with a cursor, never as a
   stream. No reply carries unbounded bytes.
6. **Outcomes and failures are different data.** What the operation did is a tool result
   (`content`, `structuredContent`, `isError`, `effects`) and keeps the codes it uses today. A
   call that could not be served is `{code, message}`; the control side returns that code and
   message as the tool result unchanged, so the two vocabularies stay one.
7. **Cancellation is a call.** The caller drops the call or sends the bridge's cancel. Cancelling
   a read of an outcome changes nothing; cancelling a process is `kill`.
8. **No per-service authorization.** Authorization is `Approval` and `Permission` over agent
   actions, and connection trust is #1296's. A face carries no credential, preset or grant.
9. **Names are camelCase.** Request, reply and error members are camelCase, as the rest of the
   migration's wire types are.

## 3. `execHost`

### 3.1 Methods

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `start` | async | `{taskId, command, cwd, workspace, roots}` | `{outputPath}` |
| `wait` | async | `{taskId, timeoutMs?}` | task record (§3.2) |
| `kill` | async | `{taskId}` | `{}` |
| `output` | async | `{taskId, cursor?, limit?}` | page (§3.4) |
| `shell` | async | `{taskId, cwd, workspace, roots, cols, rows}` | `{outputPath}` |
| `write` | async | `{taskId, data}` | `{}` |
| `resize` | async | `{taskId, cols, rows}` | `{}` |
| `info` | async | `{}` | `{home, execRoot, platformRoots, backend}` |
| `confined` | async | `{program, args, read, write, env?, timeoutMs, outputLimit, services}` | `{status, stdout, stderr, confined}` |

### 3.2 `start` and `wait`

`start` accepts only `{taskId, command, cwd, workspace, roots}`. `cwd` is a resolved working
directory — the control side obtains it from `fsHost.resolve`, which is also what triggers
[worktree recognition](#421-session-scoped-recognition-and-association) for a cwd outside the
known range; an absent `cwd` is `workspace`. `workspace` is the Session's workspace root and
`roots` the additional roots recognition confirmed and persisted. All are node-absolute. A cwd that does not exist is
`cwd_not_found`; the provider never resolves a relative path itself.

The provider prepares the boundary with `workspace` plus exactly `roots` and adds no other
writable root of its own, apart from the output and temporary directories its contract already
grants. `roots` holds the additional roots recognition confirmed; the workspace is always part of
the writable range and cannot be narrowed or replaced by leaving it out. The provider has no way
to confirm a root: a directory outside the range does not become writable because a command names
it, because `cwd` sits in it, or because the model asked. The range is the caller's confirmed
range, in the same trust position as the workspace list preparation already takes; the switches
still decide enforcement, and the model cannot set either (§3.8). It builds the shell, argv and
environment from its current shell preference, spawns, and creates the task's output file.
`taskId` is assigned by the caller (the control side allocates `exec-<uuid>` as it does today); a
second `start` with a live id is `task_exists` and starts nothing, so a retried call after a lost
reply cannot run the command twice.

`start` has no switch input. A request that still carries `sandbox`, `network` or any other
boundary switch is `invalid`: the switch is not the caller's to send
([ADR 003 D1](../adr/003-approval-scope.md), [process isolation](host-platform-isolation.md)).
`roots` is not a switch: it is the workspace range the Session already confirmed, and it grants
nothing the file facts do not already grant.

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
  its shown output matches a typical denial. Producing it is the provider's; appending it to the
  result text is the control side's.
- `timeoutMs` bounds the call's wait, not the process's obligation: reaching it kills the tree and
  answers `status: "timed_out"`. The control side computes the effective timeout from its own
  policy and passes it; a `wait` cancelled by the caller ends only the call, and the caller sends
  `kill` if it wants the process gone.
- `wait` is repeatable: a terminal record stays readable until the provider settles the task.
  Without `timeoutMs` the call waits for the end; the control side passes one whenever its policy
  has one.

### 3.3 `kill`

`kill {taskId}` terminates the task's process tree with the existing tree-kill and answers `{}`.
An ended task is a no-op that still answers `{}`; an id the provider never issued is
`task_unknown` and kills nothing. `exec_stop` stays a control-side tool: it calls `kill` and then
`wait`, and words its own `{task_id, status, result}`.

### 3.4 `output`

`output {taskId, cursor?, limit?}` reads the task's captured output by byte cursor and answers
`{text, bytes, nextCursor?, truncated, retainedFrom, reset?}`. `limit` rejects `0` (`invalid`),
defaults to 1 MiB and clamps to 4 MiB. `text` is sanitized exactly like the preview; `bytes` is
the captured bytes this page consumed; `nextCursor` is present only when the page was full and
equals `cursor + bytes`; `bytes == 0` means caught up, so the caller retries the same cursor. A
page never splits a character. For a command, the capture is the append-only output file: the
file never changes behind a cursor, `retainedFrom` is `0`, and `truncated` is the 64 MiB cap. These
are the rules of [background command output](background-task-output.md); this face adds no second
page rule, only the fields below, which a bounded capture needs. The reply never selects a path:
the task id does.

**The retained start is data, not an error.** `retainedFrom` is the earliest cursor the provider
can still serve. A capture grows at its end and drops at its start when it is bounded, so
`retainedFrom` only ever increases.

- An omitted `cursor` means `retainedFrom`, not byte zero. Asking for `{taskId}` is the reset
  request: it answers from the oldest byte the provider still holds.
- A `cursor` below `retainedFrom` is not a failure and not silent: the reply is
  `{text: "", bytes: 0, retainedFrom, nextCursor: retainedFrom, reset: true}`, and the caller
  continues at `nextCursor`. The reset is observable both as a position (`retainedFrom`) and as an
  explicit flag (`reset`), so the caller never has to infer it from a message or from an empty
  page.
- The bytes between an old cursor and `retainedFrom` are gone on purpose: the capture is bounded,
  and the reply never reconstructs them. A command's append-only output file has no such gap; only
  a bounded shell capture drops text.
- The capture may advance again between two calls. `retainedFrom` in each reply is the truth for
  that reply; a caller that reads `nextCursor` and finds it below the new `retainedFrom` takes the
  reset page and continues from there. It must not cache `retainedFrom`, and a cursor the provider
  handed out is never revised in place, only overtaken by the start.
- A lost reply is retried at the same cursor. Pages are reads of an append-only file or of an
  absolute capture offset, so a repeated read returns the same bytes or the reset page; nothing is
  consumed by reading, and no read changes the task.

### 3.5 Persistent shell

`shell {taskId, cwd, workspace, roots, cols, rows}` creates one persistent PTY shell as a task;
`write {taskId, data}` sends `data` (base64 bytes) to it, `resize` sets the geometry, `output`
reads its captured text, and `kill` ends it. `cwd` and `roots` are resolved and confirmed exactly
as for `start` (§3.2); a running shell keeps the roots it started with. `read` is not a separate
operation: one page operation with one cursor rule serves both a command's output file and the
shell's capture, and a second name for the same operation would be a duplicate. The shell's
capture is bounded (64 KiB today), so its `retainedFrom` advances as it drops the oldest text and
the reset page of §3.4 is how a caller catches up.

The command protocol that runs on top of the shell — marker pairs, occupancy, the queue and the
foreground timeout — is not part of this face. It stays with the shell's consumer, which uses
`write`/`resize`/`output` and pages, never streams (§9).

### 3.6 `info` and `confined`

`info {}` answers the provider's host facts as data: `home`, `execRoot`, `platformRoots` and
`backend` (`{state: "available" | "unavailable", reason?}`, the same decision preparation would
use). These are the values other parts read from the platform today; on a face they are a read,
not a typed getter. The exec-shell preference is not an operation here: it is a Settings document
whose public contract is `host.exec_shell.get`/`update`, and `start` reads it.

`confined` starts a host-owned helper under its own boundary — a program a plugin runs for its own
purpose, never an agent-decided command — and answers its exit status, both streams within the
limits, and whether the host could confine it. Its refusals are the ones it has today:
`InvalidPath`, `TimedOut`, `OutputLimit`, `Cancelled`, `Spawn`, `Cleanup`. It reads no setting and
grants no network.

### 3.7 Ownership, disconnect and cancellation

- The provider owns the process, the PTY, the temporary directory and the output file. The branch
  owns the task record and the facts: `exec/started` after `start` returns the id and output path,
  `exec/ended` when the terminal record is in, and the wake notice. A background handoff moves the
  control-side task record only; the process never moves.
- When the link drops, the node's own host ends the processes it started
  ([overview §6.3](overview.md#63-multiple-nodes)); the provider's task table does not survive the
  connection, so every task id is settled there. A reconnect provides the services again and
  reloads nothing: the Session keeps running.
- A control-side task that was waiting on a dropped link settles once, as failed with
  `node_unavailable`, and is never restarted; the command may have run. A `start` whose reply was
  lost is not retried with the same id.
- Cancelling an `exec` call kills the task before the call returns; a cancelled `wait` only ends
  the call. A provider that closes, or that unloads the face, kills its tasks before it stops
  serving; a node whose App Server dies leaves only the backend's parent-death handling.
- #1296 fixes the leaf's lifecycle and states the disconnect rule for owned processes. This
  document depends on that rule; it does not define reconnect or mount semantics.

### 3.8 Settings and the boundary

The provider reads its own `sandbox` and `network` switches through its `ExecutionSettings` part
at every `start` and `shell`, uses its own exec-shell preference and captured `PATH`, and prepares
the process with its own platform roots and temporary directory. A running task keeps the boundary
it started with. A missing or unreadable switch source is `isolation_config_invalid`; sandbox on
without a backend is `isolation_unavailable`; both refuse the payload before it runs, exactly as
[process isolation](host-platform-isolation.md#failure) states, and nothing falls back to a weaker
boundary.

The consumer sends no switch, reads none, and cannot widen a boundary through `cwd`, `workspace`,
`roots` or `command`: `roots` is the range the Session confirmed through the file facts, and the
provider adds nothing to it. Its tool declaration line stays what it is; it describes the
switches, not their value. On a remote node the switches are the node user's; a local management
client reads and writes them through the node's own management methods, not through this face.

### 3.9 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The provider is not connected. Only this call fails |
| `invalid` | A malformed request: relative `cwd`, `limit: 0`, a boundary field |
| `task_exists` | `start` or `shell` with a live task id |
| `task_unknown` | An id this provider never issued |
| `cwd_not_found` | The resolved `cwd` does not exist |
| `exec_shell_unavailable` | The shell preference cannot be resolved (with its own reason) |
| `isolation_unavailable` | The sandbox is on and the host has no backend |
| `isolation_config_invalid` | The switches cannot be read |
| `spawn_failed` | The process could not start |
| `output_unavailable` | The task's output file is gone |
| `cancelled` | The call was cancelled before it changed anything |

## 4. `fsHost`

### 4.1 Methods

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `resolve` | async | `{path, cwd, workspace, roots?, recognize?}` | resolution and node facts (§4.2) |
| `read` | async | `{path, offset?, limit?, encoding?}` | `{result, binding?}` (§4.3) |
| `write` | async | `{path, content, binding?}` | `{result, binding?}` |
| `edit` | async | `{path, oldText, newText, replaceAll?, binding?}` | `{result, binding?}` |
| `glob` | async | `{pattern, path, limit?}` | `{result: ToolResult}` |
| `grep` | async | `{pattern, path, include?, outputMode?, lineNumbers?, limit?, multiline?, literal?, timeoutMs?}` | `{result: ToolResult}` |
| `mentions` | async | `{path, query, limit?}` | `{candidates}` |
| `createDirectory` | async | `{path}` | `{}` |
| `replace` | async | `{path, bytes}` | `{}` |
| `list` | async | `{path}` | `{entries: [{name, kind}]}` |
| `remove` | async | `{path}` | `{}` |

The first seven are the file tools' operations; the last four are the node's storage operations
that other owners use for Session files and command output. `replace` writes bytes atomically
under the same lease discipline as today, `createDirectory` creates one directory, and `list` and
`remove` serve the sweep ([session files §3](appserver/session-files.md#3-layout-and-ownership),
§7). One face, one implementation: the file-operations owner provides all eleven.

Only `resolve` takes an unresolved `path`; the other ten take the node-absolute `path` a `resolve`
reply returned (§2 rule 3). `resolve` is also what the file facts owner and process preparation
call, so every file operation and every explicit process cwd goes through the same resolution and
recognition entry point.

### 4.2 `resolve`

`resolve {path, cwd, workspace, roots?, recognize?}` resolves `path` the way the file tools do —
`cwd`, `~`, `..` and symlinks settled, and the longest existing prefix resolved so a Session
started through a symlinked directory still matches — and answers:

```text
{path: string,             // the resolved node path the caller then uses
 workspace: boolean,       // inside `workspace`
 system: boolean,          // inside the node's platform roots
 roots: [{path, valid}],   // each passed root: contains `path` and its recorded identity still matches
 worktree?: {repository: string, root: string, subtree: string, rootIdentity: string},
 discoveryError?: string}  // recognition was asked for and failed; nothing was expanded
```

- `workspace`, `system` and `roots` are path facts of the node: it holds the filesystem, so it
  answers them. The request's `roots` entries are `{path, identity}` — the Session's confirmed
  range, each with the directory identity recorded when it was confirmed — and the reply answers
  `{path, valid}` for each; `valid: false` means the root was deleted, replaced or no longer
  contains `path`, and the caller drops it.
- `recognize: true` asks for the [worktree probe](#421-session-scoped-recognition-and-association)
  when the path is outside `workspace` and `roots`. The reply's `worktree` is a **candidate
  record**, not an expansion: the node reads Git metadata, applies the fixed-prefix mapping, and
  returns what it found; it persists nothing and grants nothing. Without `recognize` the node
  never runs Git.
- `discoveryError` reports a probe that could not answer — Git unavailable, a timeout, or
  metadata that does not verify. The Session part uses the same field when the association write
  fails on the control side. Either way nothing is expanded, so the caller keeps the existing
  approval path.
- No `worktree` and no `discoveryError` means "not the same repository or outside the eligible
  subtree": the ordinary facts decide, exactly as they do today.

The other two facts of [ADR 014](../adr/014-file-access-facts.md) are Session records, not
filesystem knowledge: `owned` is `sessionFiles.owns {sessionId, path}`
([service faces §4.5](service-faces.md#45-sessionfiles)) and `grant` is a comparison against the
Session's grant rows. Both are answered on the control side from the node's resolved `path`;
neither reads a filesystem, so neither needs the node. The control side then applies ADR 014's
table with the branch preset, and `Permission` and `Approval` stay exactly where they are.

#### 4.2.1 Session-scoped recognition and association

[Worktree access](worktree-access.md) owns the recognition rules, the mapping and the records; this
section fixes only the data and the order, because a remote node moves the Git probe away from the
Session that has to persist the result.

| Concern | Owner |
| --- | --- |
| Git probe, repository identity, fixed-prefix mapping, root directory identity | `fsHost.resolve`, on the node holding the files |
| Session view: confirmed roots with their identities, in-flight recognitions, "already known" answers, branch-independent cache | the file facts Session part — ADR 014's `fileAccessFacts` service — on the control node (worktree access §3) |
| Worktree directory record and Session usage association, deduplicated and written atomically | the `store` face's `associateWorktree` operation, provided by SessionStore (worktree access §4, R2) |
| Publishing the expanded range | the Session part, after the store acknowledges |
| Admission, asks, presets | `Permission` and `Approval`, on the control side, unchanged |

The order is fixed, and the Session part is the only participant that both waits and publishes:

1. The Session part resolves the target — or the explicit process cwd — with `resolve`, setting
   `recognize` only when its own view does not already cover the path.
2. The node answers the resolution, the node facts, and either a `worktree` candidate or a
   `discoveryError`.
3. A candidate goes to `store.associateWorktree {sessionId, repository, root, subtree,
   rootIdentity}`. That operation writes the directory record and the usage association together
   and answers `{}` only after both commit, deduplicated by repository and root and by Session and
   directory. A failed write is a `discoveryError` for the caller: the range is not published, and
   the user can still take the ordinary single-approval path.
4. Only after the acknowledgement does the Session part publish the confirmed root in its view and
   answer `workspace: true` for that path. It never expands first and persists afterwards.
5. A cancellation after the write keeps the association — it records a relationship that was
   confirmed — but the call is cancelled and nothing is published for it. A cancellation before
   the write publishes nothing.

The node keeps no Session state: the same `resolve` answers for every Session, and only the
control side knows which roots a Session confirmed. There is no Session id in a `resolve` request,
and the node cannot expand a Session's range on its own.

Later accesses re-check the root: `resolve` is called with the Session's roots and their recorded
identities, and a root whose identity no longer matches comes back `valid: false`. The Session part
drops it and re-recognizes on the next access, which is the unchanged-root rule of worktree access
§3. Session close releases the view; restore and reload start from an empty view and re-verify;
fork copies neither the view nor the associations. Session deletion releases the associations
inside its own delete transaction and keeps the directory records.

The persisted records are read only through `associateWorktree` and the store's own Session
deletion step, never by reading tables: no other part queries them for admission.

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
- `write` and `edit` are admitted before they run, so their `path` is the resolved path `resolve`
  returned. `ToolHost.resolve_write` asks `fsHost.resolve` for a remote Session instead of
  resolving locally; the resolved call is what approval and execution receive, as today.

### 4.4 `glob`, `grep` and `mentions`

- `glob {pattern, path, limit?}` walks the node's tree once and returns the bounded preview it
  returns today (at most 100 records, `limit` 1–100).
- `grep {pattern, path, include?, outputMode?, lineNumbers?, limit?, multiline?, literal?,
  timeoutMs?}` searches the node's tree once, with the existing per-file match cap, preview byte
  cap, deadline and smart-case rules.
- `mentions {path, query, limit?}` is the `session.files.search` operation on the node: it answers
  `{candidates: [{relativePath, absolutePath, dir}]}` from the node's workspace walk with the
  existing ranking, `limit` default 20 and clamp 1–50. The index behind it is the provider's own
  state, not a second face.

All three take the resolved search root in `path`; the control side resolves and admits that one
path before the call, so admission is still one decision per call and the traversal happens once,
on the node.

### 4.5 Paths, facts and approvals

A path outside the facts is refused by the same rule as today, evaluated on the node's filesystem:
the `resolve` call resolves and classifies, the control side's `Permission` decides with the branch
preset, and `Approval` asks the user when the table says to ask. No approval, preset, grant or
retry is sent to the node, and the node has no policy of its own.

Returned paths are the node's resolved absolute paths, quoted in history and stable for the file's
life. A read of a Session-owned artifact still passes file scope through the `owned` fact without a
grant, and tool approval still follows the preset
([session files §11.1](appserver/session-files.md#111-reading-a-session-file)).

### 4.6 Session files and exec output

Bytes stay on the node. `sessionFiles.put` already writes into the resource node's file storage
through `createDirectory` and `replace` and returns a node path
([session files §3.1](appserver/session-files.md#31-on-disk)); this face reads and writes those
files like any other node file, and `sessionFiles` remains the owner of the rows. An exec task's
output file is the node's, and `execHost.output` is how it is paged. Neither moves bytes through
the control side.

### 4.7 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The provider is not connected. Only this call fails |
| `invalid` | A relative path, an unbounded page, a malformed request |
| `not_found` | A storage operation on a path that does not exist, where the operation requires one |
| `locked` | A storage operation whose lease is held by another call |
| `cancelled` | The call was cancelled before it changed anything |

Operation refusals are not in this table: they arrive as the tool result's own `isError` with the
code the operation already uses, so one code vocabulary serves the local and the remote path.

## 5. `store`

`store` is the Session state face. It is one name with two operation families, provided by the
daemon that owns the Session:

| Family | Operations |
| --- | --- |
| Timeline | `append`, `head`, `branches`, `readPage`, `readSegments`, `query`, `trace`, `batchEnd`, `saveSnapshot`, `latestSnapshot` |
| Session rows | `createSession`, `sessionConfig`, `releaseSession`, `rollbackSession`, `associateWorktree`, and the row-set operations for sessions, owners, branches, content, snapshots, grants, metadata, settings, providers, agents and Session-file rows, with `fork` and `delete` as one transaction each |

- `append` takes one atomic batch and answers after the batch is durable, so the publish step is
  the reply and no callback crosses. `head`, `readPage` and the rest keep the cursor and batch
  boundary rules of [timeline storage](../architecture/rust-agent/timeline-storage.md).
- `associateWorktree {sessionId, repository, root, subtree, rootIdentity}` writes the worktree
  directory record and the Session usage association as one commit and answers `{}` after it, so
  the reply is the acknowledgement the file facts owner waits for before publishing a confirmed
  root (§4.2.1). Its deduplication, its place in Session deletion and its format decision belong
  to [worktree access §4](worktree-access.md#4-持久使用关联与清理边界) and the storage owner.
- The Session-row operations are the repository operations of
  [#1298](https://github.com/arcships/dim/issues/1298); that issue's list and field sets are the
  contract, and this document does not pre-empt them.
- Both SQLite and PostgreSQL implement the same face. Which engine answers is configuration of
  the state owner, not a difference the caller sees; a Session whose state is unreachable is a
  failed call, and the Session layer's own rule decides whether it stops
  ([#1298](https://github.com/arcships/dim/issues/1298)).
- One Session has one writer, and the store face carries no lease, takeover or second-writer
  operation: that rule belongs to the Session owner, above the store.

The Rust `TimelineStore` trait and the SQL engine are the local binding and implementation of this
face, not a second contract (§6).

## 6. Provider and binding, not a second contract

| Old name | Face | What the name is now |
| --- | --- | --- |
| `hostPlatform` | `execHost` | The provider and its Rust binding: `ProcessTree`, `PtyProcessTree`, temporary directories and shell preparation are its internals, reached through the operations |
| `fileStorage` | `fsHost` | The provider and its binding: roots, leases and one validated path component per open are its internals |
| `sqliteStorage` | `store` | The SQL engine inside the provider |
| `timelineStore` | `store` | The Timeline half of the face; the `TimelineStore` trait is its binding |

Each of these is reached by Rust callers through a thin binding over the operations — a direct
in-process call rather than JSON — and none of them publishes a second API: there is no owner
trait beside the face, no object type another module constructs, and no method set that the face
does not have. A consumer that needs something the face does not offer needs a new operation on
the face, not a new accessor beside it.

## 7. Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Each face belongs to the part that owns the resource or the state; consumers stay in their own scope. No new Capability on the control side |
| §4 rule 1 | Command events are not added. A background command's fact is still `exec/started`/`exec/ended` on its branch, written by its owner |
| §4 rule 2 | A handoff keeps its fact on the branch and its process with the provider; nothing is handed to a second owner |
| §4 rule 3 | No before callback and no commit-time I/O: no face operation runs inside the commit lock |
| §4 rule 4 | Core, assembly and the Timeline carry no remote-execution or engine branch; the provider is chosen by deployment |
| §4 rule 5 | No batch semantics change: `store.append` takes one atomic batch and answers after it is durable |
| §4 rule 6 | File content and command output reach the model only through tool results, as they do today |
| §4 rule 7 | Consumers use the faces; they read no provider internals, and the providers use only their own host facilities |
| §4 rule 8 | Existing mechanisms suffice. The host side's per-call service resolution is [#861](https://github.com/arcships/dim/issues/861), already raised by [overview §6.3](overview.md#63-multiple-nodes); this document raises nothing new |
| §4 rule 9 | No new Timeline fact: `exec/started` and `exec/ended` already exist, and the faces write none |
| ADR 002 D2 | No wiring plugin: each face is its provider's own service |
| ADR 002 D3 | Dependencies follow scope: `execHost` and `fsHost` are process names a leaf can provide; `store` is provided where the Session state is |
| ADR 002 D4 | Business configuration is read at use time: the switches at each `start`, the shell preference at each spawn, the engine at open |
| ADR 002 D5 | §3–§5 are the language-neutral contract; the Rust bindings implement them |
| ADR 003 D1, D5 | No boundary input in a call; a preset, approval or replay confers none |
| ADR 009 | Permission decides; `execHost`'s provider enforces |
| ADR 010, ADR 011 | The switches stay the host user's settings, read and enforced by the provider |
| ADR 012, ADR 013 | Grants and Session-file ownership stay control-side records; paths are the node's |
| ADR 014 | The four facts keep their sources: resolution, workspace, system and the worktree probe are the node's, the row facts the control node's. `resolve` never publishes an expansion; the Session part publishes only after the store acknowledged the association (§4.2.1) |
| Worktree access R1 | The Session part answers "known range" from its own view before it asks the node to probe; the probe is one extra field on the same call, not a second facts service |
| Worktree access R3 | Process preparation resolves its explicit cwd through `resolve` before `start`/`shell`, and hands the confirmed range as `roots`; a running process keeps what it started with |

## 8. Requirements raised to mechanism owners

None beyond what is already raised. [#861](https://github.com/arcships/dim/issues/861) is the one
runtime dependency: the host side resolves a Session's process-scope service per call, by the
Session's resource node, which [overview §6.3](overview.md#63-multiple-nodes) already states as a
requirement on the runtime and on rutis. The store's repository operations and
`associateWorktree` are [#1298](https://github.com/arcships/dim/issues/1298)'s own work, and the
recognition records are [worktree access](worktree-access.md) R2's. Neither face needs a new event,
a batch change, a synchronous callback across a link, or a Timeline rule.

## 9. Open decisions

1. **Naming and instance per leaf.** Process-scope names are global, so `execHost` and `fsHost`
   need one instance per leaf: `execHost.<leaf>` or a host-side route.
   [#1296](https://github.com/arcships/dim/issues/1296) decides; this document assumes only that a
   Session's calls reach one provider.
2. **Disconnect policy.** The node ends the processes it started, per overview §6.3. #1296 states
   and tests the rule for mounted resource leaves; the control-side settlement
   (`node_unavailable`, no restart) follows from it.
3. **`owned` on the control side.** The fact is a row lookup, so it is answered by `sessionFiles`
   with the node's resolved path rather than on the node. If review wants it computed with the
   other three, the rows would have to move, which is #1298's kind of decision.
4. **Where the persistent-shell protocol runs.** `write`/`resize`/`output` are enough for a
   control-side marker and occupancy protocol that pages its output; running the protocol on the
   node would make it `start`/`wait` instead. #1295 decides when it wires PTY, after foreground
   and background.
5. **Result text ownership.** File operations return their tool result from the provider because
   the text is the file operation's own (line numbers, previews, image blocks). A command returns
   records, and the control side composes the tool result, because the task identity, the
   background policy and the output-path registration are its. If a migration agent finds the
   split costs more than it saves, the alternative is a command tool result built by the provider
   plus a control-side registration step — but only one of the two is chosen, not both.
6. **One `store` name or two.** The Timeline family and the Session-row family are one face with
   two families here because one repository port serves both (#1298). If the implementation finds
   two names clearer, the split is a naming decision on that PR, not a new contract.
7. **Root identity form.** `roots` entries carry the directory identity a confirmed root was
   recorded with, and `resolve` compares it on the node. What that identity is (device and inode,
   file id, something else) is the provider's platform detail; the caller treats it as an opaque
   string and only compares it for equality through the `valid` answer.

## 10. Ablation

The faces are operations over data with ids, not adapters: `start`/`wait`/`kill`/`output` reuse the
existing process tree, capture, page rule and fault vocabulary; `shell`/`write`/`resize` are the
PTY primitives the shell already has; `info` and `confined` are reads and a start the platform
already performs. `fsHost` adds no primitive file API, no per-file traversal, and no policy of its
own: the raw storage operations are the four the node's storage already offers, and the `resolve`
call is needed because a path's facts cannot be computed on a machine that does not hold the file.
`store` publishes the repository operations the state owner already has, so the SQL engine and the
`TimelineStore` trait stop being second contracts instead of gaining a wrapper.

Three details were added because the remote split leaves no other place for them, and each replaces
something that would otherwise be guesswork: the `worktree` candidate field on `resolve` (the
probe must run where the files are, while the association must commit where the rows are), the
`retainedFrom`/`reset` page (an error cannot carry the cursor, and an omitted cursor cannot mean
zero for a bounded capture), and the resolved-`cwd` rule for `start`/`shell` (a caller cannot
resolve a node path, and recognition has to happen before the process starts). Nothing else was
added that a caller could not do without.
