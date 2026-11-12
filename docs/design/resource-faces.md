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
| `execHost` | `start`, `wait`, `kill`, `settle`, `output`, `tail`, `removeOutputDirectory`, `shell`, `write`, `resize`, `info`, `confined`, `configured` | the process owner | #1295 |
| `fsHost` | tool family: `resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions`; storage family: `stat`, `readBytes`, `openRead`, `readAt`, `closeRead`, `lock`, `unlock`, `backup`, `createDirectory`, `replace`, `list`, `remove` | the file-operations owner | #1297 |
| `store` | Timeline and Session-row repository operations, and the commit notification | the state owner | #1298 |

```text
 control node                                        resource leaf (mounted per #1296)
   WorkspaceTools          ── fsHost ─── data ──────►  file operations on node files
   exec, ShellSession      ── execHost ─ data ──────►  processes, PTY, output files
   settings, auth, Hooks   ── fsHost ─── data ──────►  the same face, answered locally
   Permission and Approval                            never asked, never decides
   Timeline facts, task records, Session-file rows
   Session state owner     ── store ──── data ──────►  the same face, answered locally
```

**A face is a shape; the mount is a deployment.** `execHost` and `fsHost` are provided by the
daemon that holds the process, the file or the session's own configuration — the control node's
own plugins, or a resource leaf mounted per
[#1296](https://github.com/arcships/dim/issues/1296). `store` is provided where the Session state
is, always in the daemon that owns that Session
([#1298](https://github.com/arcships/dim/issues/1298)); it is the same face whether the rows live in
SQLite or PostgreSQL. Whether a face is reachable over a link is decided by deployment, not by a
classification on the name.

**The target rule, and its decision dependency.** This document is written against one contract
per service: the operations are the public contract, and a Rust typed client is a binding over
them, not a second API. The policy text in [service faces](service-faces.md) §1, §2.4, §3.5–§3.7
and §4.1–§4.2 still classifies the rest of the catalog with `internal` reasons, and
[#1393](https://github.com/arcships/dim/pull/1393) keeps justified reasons for other names; this
document does not amend that policy, and nothing outside these three faces follows from it. Until
the maintainer decides that policy ([#1292](https://github.com/arcships/dim/issues/1292)), treat
this section as the target these faces are designed against, and the policy document as
authoritative for every other name. What it settles here: a shape that used to be an object is
reshaped into operations; a credential is a capability an operation uses and never returns; a hot
path is a property of the consumer, not a reason to publish a second API. The old names —
`hostPlatform`, `fileStorage`, `sqliteStorage`, `timelineStore` — are the providers and their Rust
bindings (§6).

What stays on the control side, in every case: Session and branch scope, admission
(`Approval`, and `Permission` with the [ADR 014](../adr/014-file-access-facts.md) table), the
`exec/started` and `exec/ended` facts and the wake notice, the task record, Session-file rows,
the worktree Session view, and the recorded tool result. No face asks the user anything, decides
whether a call may run, or consults who is calling.

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
   `shell` included, takes a path a `resolve` reply or the caller's own record already returned.
   Returned paths are stable for history: the same file always resolves to the same path.
4. **Ids are values.** A task, read or lease id is a string the provider issues or the caller
   assigns; ids carry no authority.
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
| `wait` | async | `{taskId, timeoutMs?}` | terminal record (§3.2) |
| `kill` | async | `{taskId}` | `{}` |
| `settle` | async | `{taskId}` | `{}` |
| `output` | async | `{taskId, cursor?, limit?} \| {outputPath, cursor?, limit?}` | page (§3.4) |
| `tail` | async | `{taskId}` | `{text, totalBytes, truncated}` (§3.5) |
| `removeOutputDirectory` | async | `{path}` | `{}` (§3.8) |
| `shell` | async | `{taskId, cwd, workspace, roots, cols, rows}` | `{outputPath}` |
| `write` | async | `{taskId, data}` | `{}` |
| `resize` | async | `{taskId, cols, rows}` | `{}` |
| `info` | async | `{}` | host facts (§3.7) |
| `confined` | async | `{program, args, read, write, env?, timeoutMs, outputLimit, services}` | `{status, stdout, stderr, confined, stdoutTruncated, stderrTruncated}` |
| `configured` | async | `{command, cwd, env?, stdin?, timeoutMs, limits}` | `{status, exitCode?, signal?, durationMs, stdout, stderr, stdoutTruncated, stderrTruncated}` |

### 3.2 `start` and `wait`

`start` accepts only `{taskId, command, cwd, workspace, roots}`. `cwd` is a resolved working
directory — the control side obtains it from `fsHost.resolve`, which is also what triggers
[worktree recognition](#421-session-scoped-recognition-and-association) for a cwd outside the
known range; an absent `cwd` is `workspace`. `workspace` is the Session's workspace root and
`roots` the mapped subtree paths from the Session's confirmed worktrees. All are node-absolute.
A cwd that does not exist is `cwd_not_found`; the provider never resolves a relative path itself.

The provider prepares the boundary with `workspace` plus exactly `roots` and adds no other
writable root of its own, apart from the output and temporary directories its contract already
grants. `roots` holds the additional roots recognition confirmed; the workspace is always part of
the writable range and cannot be narrowed or replaced by leaving it out. The provider has no way
to confirm a root: a directory outside the range does not become writable because a command names
it, because `cwd` sits in it, or because the model asked. The range is the caller's confirmed
range, in the same trust position as the workspace list preparation already takes; the switches
still decide enforcement, and the model cannot set either (§3.9). It builds the shell, argv and
environment from its current shell preference, spawns, and creates the task's output file.
`taskId` is assigned by the caller (the control side allocates `exec-<uuid>` as it does today); a
second `start` with an id the provider still holds is `task_exists` and starts nothing, so a
retried call after a lost reply cannot run the command twice.

`start` has no switch input. A request that still carries `sandbox`, `network` or any other
boundary switch is `invalid`: the switch is not the caller's to send
([ADR 003 D1](../adr/003-approval-scope.md), [process isolation](host-platform-isolation.md)).
`roots` is not a switch: it is the workspace range the Session already confirmed, and it grants
nothing the file facts do not already grant.

`wait` returns the terminal record once the process tree has ended:

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
- `wait` is repeatable and stays readable until the task is settled (§3.8). Without `timeoutMs` the
  call waits for the end; the control side passes one whenever its policy has one.

### 3.3 `kill`

`kill {taskId}` terminates the task's process tree with the existing tree-kill and answers `{}`.
An ended but unsettled task is a no-op that still answers `{}`; an id the provider does not hold is
`task_unknown` and kills nothing. `exec_stop` stays a control-side tool: it calls `kill`, then
`wait`, then `settle`, and words its own `{task_id, status, result}`. `kill` answering
`task_unknown` means the task was already settled, so `exec_stop` answers from the branch's own
task record — `exec_task_unknown` only when the branch has no record either.

### 3.4 `output`

`output` reads captured output by byte cursor. A request supplies exactly one of `taskId` and
`outputPath`. Both forms answer
`{text, bytes, nextCursor?, truncated, retainedFrom, reset?}`. `limit` rejects `0` (`invalid`),
defaults to 1 MiB and clamps to 4 MiB. `text` is sanitized exactly like the preview; `bytes` is
the captured bytes this page consumed; `nextCursor` is present when the page was full and equals
`cursor + bytes`; `bytes == 0` means caught up, so the caller retries the same cursor. A page
never splits a character. For a command, the capture is the append-only output file: the file
never changes behind a cursor, `retainedFrom` is `0`, and `truncated` is the 64 MiB cap. These are
the rules of [background command output](background-task-output.md); this face adds no second page
rule, only the fields below, which a bounded capture needs.

- `{taskId, cursor?, limit?}` selects a task held by the provider. This serves the PTY capture
  and command output while the task record is held.
- `{outputPath, cursor?, limit?}` pages a command's file independently of the task record.
  For `session.exec.output.page`, the control side resolves the task in the requested Session
  and branch and takes the path from that branch's `exec/started`. This route serves ended and
  rebuilt tasks, including after settlement or provider restart. The provider opens only files
  within its command-output root and checks containment when opening. The control side uses
  the terminal fact's `outputTruncated` when present. On the path-only read, the provider uses
  the file-size-at-cap rule for recovered tasks. The public RPC continues to take a task id.

**The retained start is data, not an error.** `retainedFrom` is the earliest cursor the provider
can still serve. A bounded capture grows at its end and drops at its start, so `retainedFrom` only
ever increases.

- An omitted `cursor` means `retainedFrom`, not byte zero. Asking for `{taskId}` is the reset
  request: it answers from the oldest byte the provider still holds.
- A `cursor` below `retainedFrom` is not a failure: the reply is
  `{text: "", bytes: 0, retainedFrom, reset: true}` and carries **no** `nextCursor`. The
  continuation is `retainedFrom`: `reset` overrides the `bytes == 0` rule above, which otherwise
  means "retry the same cursor". A client that checks `reset` first never loops.
- The bytes between an old cursor and `retainedFrom` are gone on purpose; the reply never
  reconstructs them. A command's append-only output file has no such gap; only a bounded shell
  capture drops text.
- The capture may advance again between two calls. `retainedFrom` in each reply is the truth for
  that reply; a caller whose next cursor is below the new `retainedFrom` takes the reset page and
  continues from there. It must not cache `retainedFrom`, and a cursor the provider handed out is
  never revised in place, only overtaken by the start.
- A lost reply is retried at the same cursor. Pages are reads of an append-only file or of an
  absolute capture offset, so a repeated read returns the same bytes or the reset page; nothing is
  consumed by reading, and no read changes the task.

### 3.5 `tail`

`tail {taskId}` answers the live tail a task shows while it runs:
`{text, totalBytes, truncated}`. `text` is the last 2,000 lines within 50 KiB of the capture,
sanitized like the preview; `totalBytes` is how many bytes the capture holds so far, so a client
can tell growth from a stall; `truncated` is the same flag the terminal record carries. It is a
read of the live capture, not a page: the control side's `session.exec.output` uses it, and a
client that needs the whole output pages `output` from `retainedFrom`. Once the live capture is
released, a held task answers `output_unavailable`. The control side maps that and `task_unknown`
to the existing `exec_task_unknown` response for this RPC. The terminal preview comes from
`exec/ended` under the [background output contract](background-task-output.md#2-client-method).

### 3.6 Persistent shell

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
`write`/`resize`/`output` and pages, never streams (§9). A marker that never arrives is not proof
that a command still runs: on a lost link the provider ends the task (§3.8), so the consumer
settles the command as ended with the task's loss, never as still running.

### 3.7 `info`, `confined` and `configured`

`info {}` answers the provider's host facts as data, because other owners read them today:

```text
{os: "darwin" | "linux" | "win32",
 home: string, timezone?: string, execRoot: string, platformRoots: [string],
 backend: {state: "available"} | {state: "unavailable", reason: string},
 execShell: {state: "available", executable: string, edition: string,
             source: string, outputEncoding: "utf8" | "unknown"}
          | {state: "unavailable", code: string, reason: string}}
```

`backend` is the decision preparation would use; `execShell` is the current resolved shell
preference, which the environment prompt source reads on every load. The exec-shell preference is
otherwise a Settings document whose public contract is `host.exec_shell.get`/`update`; `start`
reads it, and this face does not write it.

`confined` starts a host-owned helper under its own boundary — a program a plugin runs for its own
purpose, never an agent-decided command — and answers its exit status, both streams within the
limits, whether the host could confine it, and whether either stream was cut. Its refusals are the
ones it has today: `InvalidPath`, `TimedOut`, `OutputLimit`, `Cancelled`, `Spawn`, `Cleanup`. It
reads no setting and grants no network.

`configured` runs a **configuration command**: a command that exists because of configuration, not
because an agent decided it (a Hooks handler,
[Hooks A.6.3](plugins/Hooks.md#a63-process-start-with-an-initial-stdin)). It starts the command
through the same shell spec as `start`, writes `stdin` (base64 bytes, if any) to the child's stdin
and closes it, adds `env` entries to the spec environment, and answers the exit status and both
streams within `limits: {stdout, stderr}`. It reads **no** isolation switch and prepares **no**
sandbox: a configuration command runs outside the agent boundary, exactly as it does today
([ADR 003 D2](../adr/003-approval-scope.md)). It takes no task id, no output file and no
background handoff; a cancelled call or `timeoutMs` kills the tree before the call answers. It is
not an agent-reachable operation: no tool maps to it.

### 3.8 Ownership, disconnect, settlement and cancellation

- The provider owns the process, the PTY, the temporary directory and the output file. The branch
  owns the task record and the facts: `exec/started` after `start` returns the id and output path,
  `exec/ended` when the terminal record is in, and the wake notice. A background handoff moves the
  control-side task record only; the process never moves.
- **A start that cannot be recorded kills its process.** If appending `exec/started` fails, or the
  call is cancelled after `start` returned, the control side calls `kill` and waits for the tree to
  end before returning the tool error: no process outlives an unrecorded start. A start whose
  reply never arrived is not retried — the branch has no record of it, and the id stays held until
  the connection ends, so a retry cannot run the command twice.
- **Settlement.** The provider holds a task record from `start` until `settle`. `start` and
  `shell` refuse every id still held, live or terminal (`task_exists`). After recording the
  terminal fact, the control side can call `settle` to release the task record and its runtime
  resources. Calls by task id then answer `task_unknown` until the id is reused. A new task
  creates a fresh output file. Settlement, including on disconnect, preserves output files.
  The control side can still page the registered command output by path (§3.4).
- **Output directory removal.** SessionStore applies the last-owner rule and orders durable
  directory removal before the row commit under the
  [Session deletion contract](appserver/session-deletion-and-fork.md#32-sequence). It calls
  `execHost.removeOutputDirectory {path}` for persistent-shell output. The provider validates
  that the directory is strictly below its `execRoot`, removes it and makes removal durable
  before replying, as required by [ADR 013 D4](../adr/013-command-output-ownership.md#d4-fork-delete-and-growth).
  An absent directory counts as removed. SessionStore uses `fsHost.remove` for command-output
  directories in the FileStorage root.
- When the link drops, the node's own host ends the processes it started
  ([overview §6.3](overview.md#63-multiple-nodes)), and settlement is the connection's end. A
  reconnect provides the services again and reloads nothing: the Session keeps running.
- A control-side task that was waiting on a dropped link settles once, as failed with
  `node_unavailable`, and is never restarted; the command may have run.
- Cancelling an `exec` call kills the task before the call returns; a cancelled `wait` only ends
  the call. A provider that closes, or that unloads the face, kills its tasks before it stops
  serving; a node whose App Server dies leaves only the backend's parent-death handling.
- #1296 fixes the leaf's lifecycle and states the disconnect rule for owned processes. This
  document depends on that rule; it does not define reconnect or mount semantics.

### 3.9 Settings and the boundary

The provider reads its own `sandbox` and `network` switches through its `ExecutionSettings` part
at every `start` and `shell`, uses its own exec-shell preference and captured `PATH`, and prepares
the process with its own platform roots and temporary directory. A running task keeps the boundary
it started with. A missing or unreadable switch source is `isolation_config_invalid`; sandbox on
without a backend is `isolation_unavailable`; both refuse the payload before it runs, exactly as
[process isolation](host-platform-isolation.md#failure) states, and nothing falls back to a weaker
boundary. `configured` and `confined` read no switch: the first runs outside the boundary by
design, the second under its own.

The consumer sends no switch, reads none, and cannot widen a boundary through `cwd`, `workspace`,
`roots` or `command`: `roots` is the range the Session confirmed through the file facts, and the
provider adds nothing to it. Its tool declaration line stays what it is; it describes the
switches, not their value. On a remote node the switches are the node user's; a local management
client reads and writes them through the node's own management methods, not through this face.

### 3.10 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The provider is not connected. Only this call fails |
| `invalid` | Malformed request or an output path outside the operation's root |
| `task_exists` | `start` or `shell` with an id the provider still holds |
| `task_unknown` | An id the provider does not hold: never issued, or settled |
| `cwd_not_found` | The resolved `cwd` does not exist |
| `exec_shell_unavailable` | The shell preference cannot be resolved (with its own reason) |
| `isolation_unavailable` | The sandbox is on and the host has no backend |
| `isolation_config_invalid` | The switches cannot be read |
| `spawn_failed` | The process could not start |
| `output_unavailable` | The selected output file is missing or unreadable, or `tail` has no live capture for a held task |
| `remove_failed` | Removing a persistent-shell output directory or making the removal durable failed |
| `cancelled` | The call was cancelled before it changed anything |

## 4. `fsHost`

### 4.1 Methods

Tool family — the file tools' operations, one call each:

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `resolve` | async | `{path, cwd, workspace, roots?, recognize?}` | resolution and node facts (§4.2) |
| `read` | async | `{path, offset?, limit?, encoding?}` | `{result, binding?}` (§4.3) |
| `write` | async | `{path, content, binding?}` | `{result, binding?}` |
| `edit` | async | `{path, oldText, newText, replaceAll?, binding?}` | `{result, binding?}` |
| `glob` | async | `{pattern, path, limit?}` | `{result: ToolResult}` |
| `grep` | async | `{pattern, path, include?, outputMode?, lineNumbers?, limit?, multiline?, literal?, timeoutMs?}` | `{result: ToolResult}` |
| `mentions` | async | `{path, query, limit?}` | `{candidates}` |

Storage family — the raw operations the other file owners use. They take absolute paths and no
tool semantics: no line numbers, no images, no effects.

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `stat` | async | `{path}` | `{kind: "file" \| "directory" \| "missing" \| "other", size?}` |
| `readBytes` | async | `{path, limit, leaseId?}` | `{bytes, size}` |
| `openRead` | async | `{path}` | `{readId, size}` |
| `readAt` | async | `{readId, offset, limit}` | `{bytes}` |
| `closeRead` | async | `{readId}` | `{}` |
| `lock` | async | `{path}` | `{leaseId}` |
| `unlock` | async | `{leaseId}` | `{}` |
| `backup` | async | `{leaseId, suffix, permissions?}` | `{}` |
| `createDirectory` | async | `{path}` | `{}` |
| `replace` | async | `{path, bytes, permissions?, leaseId?}` | `{}` |
| `list` | async | `{path}` | `{entries: [{name, kind}]}` |
| `remove` | async | `{path}` | `{}` |

- `readBytes` is the bounded read: `limit` is required, a larger file is `too_large` without
  partial bytes, and `bytes` is base64. `size` is the file's length, so a caller can tell "done"
  from "cut".
- `readBytes` and `replace` accept a `leaseId`: they then read or write through that lease, so a
  read-modify-write keeps one lock from read to replace. `backup` writes the lease's current bytes
  to a durable sibling named `<name>.<suffix>`. `unlock` releases the lease; ending the caller's
  connection releases every lease it held. `permissions` is data — `"private"` (owner-only, 0600
  on Unix) or `"preserve"` (the existing file's mode) — because the provider's own permission type
  is an OS object and does not cross; absent means `"preserve"`, as today. `replace` also accepts
  `"privateReadOnly"` for a newly written Session file (Unix mode `0400`, Windows read-only
  attribute).
- `openRead` pins a file for several bounded random reads (`readAt`) and is closed by `closeRead`
  or the caller's connection. UI resource streaming uses these bounded reads. Plugin-file
  uploads and work directories stay with the host-local
  [PluginFiles service](../../crates/dim-app-server/README.md#trusted-plugin-storage).
- `stat` is one path, never a traversal: existence and kind for configuration discovery (a
  project rule file, a hooks file). It is not a `stat`/`open` primitive API; traversals stay
  `glob`, `grep` and `mentions`.
- `createDirectory` creates one directory with the existing rules (mode 0700, an existing one
  counts as created, the parent must exist), `list` and `remove` serve the Session-file sweep, and
  `replace` without a lease is the atomic whole-file write.

Only `resolve` takes an unresolved `path`; every other operation takes the node-absolute `path`
its caller holds (§2 rule 3). `resolve` is what the file facts owner and process preparation
call, so every file operation and every explicit process cwd goes through the same resolution and
recognition entry point.

### 4.2 `resolve`

`resolve {path, cwd, workspace, roots?, recognize?}` resolves `path` the way the file tools do —
`cwd`, `~`, `..` and symlinks settled, and the longest existing prefix resolved so a Session
started through a symlinked directory still matches — and answers:

```text
{path: string,                        // the resolved node path the caller then uses
 workspace: boolean,                  // inside `workspace`
 system: boolean,                     // inside the node's platform roots
 roots: [{root, contains, valid}],    // per passed root; see below
 worktree?: {repository: string, root: string, subtree: string, rootIdentity: string},
 discoveryError?: string}             // recognition was asked for and failed; nothing was expanded
```

- `workspace`, `system` and `roots` are path facts of the node: it holds the filesystem, so it
  answers them.
- `roots` entries are `{root, subtree, rootIdentity}` in the request and `{root, contains, valid}`
  in the reply. `subtree` is the fixed mapped prefix within the worktree. **`contains` and `valid`
  are separate answers**: `contains` checks the resolved target against `subtree`. `valid` checks
  the directory's identity at `root` and verifies that the mapping still resolves to that prefix
  under the [worktree mapping rules](worktree-access.md#22-仓库身份与目录范围).
  The caller drops a root only when `valid` is false; a false `valid` for one root never discards
  another root the Session confirmed.
- `recognize: true` asks for the [worktree probe](#421-session-scoped-recognition-and-association)
  when the path is outside `workspace` and `roots`. The reply's `worktree` is a **candidate
  record**, not an expansion: the node reads Git metadata, applies the fixed-prefix mapping, and
  returns the root, mapped subtree and opaque directory identity. The Session keeps
  `rootIdentity` in memory for revalidation (§4.2.2). Without `recognize` the node never runs Git.
- `discoveryError` reports a probe that could not answer — Git unavailable, a timeout, or
  metadata that does not verify. The Session part uses the same field when the association write
  fails on the control side (§4.2.1). Either way nothing is expanded, so the caller keeps the
  existing approval path.
- No `worktree` and no `discoveryError` means "not the same repository or outside the eligible
  subtree": the ordinary facts decide, exactly as they do today.

The other two facts of [ADR 014](../adr/014-file-access-facts.md) are Session records, not
filesystem knowledge: `owned` is `sessionFiles.owns {sessionId, path}`
([service faces §4.5](service-faces.md#45-sessionfiles)) and `grant` is a comparison against the
Session's grant rows. Both are answered on the control side from the node's resolved `path`;
neither reads a filesystem, so neither needs the node. The control side then applies ADR 014's
table with the branch preset, and `Permission` and `Approval` stay exactly where they are.

#### 4.2.1 Session-scoped recognition and association

[Worktree access](worktree-access.md) owns the recognition rules, the mapping, the records and the
persistence format decision; this section fixes only the data and the order, because a remote node
moves the Git probe away from the Session that has to persist the result.

| Concern | Owner |
| --- | --- |
| Git probe, repository identity, fixed-prefix mapping, root directory identity | `fsHost.resolve`, on the node holding the files |
| Session view: confirmed roots with their identities, in-flight recognitions, "already known" answers, branch-independent cache | the file facts Session part — ADR 014's `fileAccessFacts` service — on the control node (worktree access §3) |
| Worktree directory record and Session usage association, deduplicated and written atomically | the `store` face's `associateWorktree` operation, provided by SessionStore (worktree access §4, R2) |
| Publishing the expanded range, and its revalidation | the Session part, after the store acknowledges (§4.2.2) |
| Admission, asks, presets | `Permission` and `Approval`, on the control side, unchanged |

The order is fixed, and the Session part is the only participant that both waits and publishes:

1. The Session part resolves the target — or the explicit process cwd — with `resolve`, setting
   `recognize` only when its own view does not already cover the path.
2. The node answers the resolution, the node facts, and either a `worktree` candidate or a
   `discoveryError`.
3. A candidate goes to `store.associateWorktree {sessionId, repository, root, subtree}`. That
   operation writes the directory record and the usage association together and answers `{}` only
   after both commit, deduplicated by repository and root and by Session and directory. A failed
   write is a `discoveryError` for the caller: the range is not published, and the user can still
   take the ordinary single-approval path.
4. Only after the acknowledgement does the Session part revalidate the candidate and publish it
   (§4.2.2), then answer `workspace: true` for the path. It never expands first and persists
   afterwards.
5. A cancellation after the write keeps the association — it records a relationship that was
   confirmed — but the call is cancelled and nothing is published for it. A cancellation before
   the write publishes nothing.

The node keeps no Session state: the same `resolve` answers for every Session, and only the
control side knows which roots a Session confirmed. There is no Session id in a `resolve` request,
and the node cannot expand a Session's range on its own.

Session close releases the view; restore and reload start from an empty view and re-verify; fork
copies neither the view nor the associations. Session deletion releases the associations inside
its own delete transaction and keeps the directory records. The persisted records are read only
through `associateWorktree` and the store's own Session deletion step, never by reading tables: no
other part queries them for admission.

#### 4.2.2 Revalidation before publication

The candidate is a filesystem observation made before a storage wait. It must still be true when
it is published, or it must not be published:

1. After `associateWorktree` returns, the Session part checks its own lifecycle: a Session that is
   closing, deleted or between generations publishes nothing and answers the call with its normal
   closing or cancelled outcome. A committed association stays; only publication stops.
2. It revalidates the candidate with a second `resolve` — `recognize: false`, so no Git probe and
   no new grant — passing the candidate's `{root, subtree, rootIdentity}` in `roots` and the target
   path. Before publishing, the Session part checks that the same generation is still active and
   the call is still live, using step 1's outcome if either check fails.
3. Publication requires `valid: true`, `contains: true` and the unchanged resolved path. If the
   reply fails this check, the Session drops the candidate, keeps the committed association and
   answers with the unexpanded facts. A failed `resolve` call returns its error without publishing.

Waiting for persistence is not a fresh observation: the recheck is the only thing that lets the
earlier probe become a published range, and a stale candidate is never published from the old
answer.

Later accesses re-check the root: `resolve` is called with the Session's roots and their recorded
identities, and a root whose `valid` is false is dropped and re-recognized on the next access,
which is the unchanged-root rule of worktree access §3. A root that merely does not contain the
current path is not dropped.

### 4.3 `read`, `write` and `edit`

The reply is the operation's own tool result, plus the file binding when the operation established
or changed one:

```text
{result: ToolResult,            // content, structuredContent, isError, effects
 binding?: TextFileBinding}
```

`TextFileBinding` is the existing type, exactly as the tool result already serializes it
(`crates/dim-agent-types/tests/fixtures/baseline/types/tool.ts`):

```text
{rawRevision: string,                                   // SHA-256 of the raw bytes, lowercase hex
 encoding: {kind: "utf8"} | {kind: "utf16le"} | {kind: "utf16be"}
         | {kind: "legacy", name: string},
 encodingSource: "bom" | "workspace" | "user" | "utf8-policy",
 bom: "none" | "utf8" | "utf16le" | "utf16be",
 eol: "none" | "lf" | "crlf" | "mixed"}
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
the `resolve` call resolves and classifies, the control side's `Permission` decides with the
branch preset, and `Approval` asks the user when the table says to ask. No approval, preset, grant
or retry is sent to the node, and the node has no policy of its own.

Returned paths are the node's resolved absolute paths, quoted in history and stable for the file's
life. A read of a Session-owned artifact still passes file scope through the `owned` fact without a
grant, and tool approval still follows the preset
([session files §11.1](appserver/session-files.md#111-reading-a-session-file)).

### 4.6 Session files and exec output

`sessionFiles.put` uses `createDirectory` and `replace` with `permissions: "privateReadOnly"` to
store the file on the resource node. It returns a node path and owns the rows under the
[Session-file contract](appserver/session-files.md#31-on-disk). Command output stays on the node
and is read through `execHost.output` and `execHost.tail` (§3.4–§3.5).

### 4.7 Errors

| Code | When |
| --- | --- |
| `node_unavailable` | The provider is not connected. Only this call fails |
| `invalid` | A relative path, an unbounded request, an id the provider does not hold |
| `not_found` | A storage operation on a missing path where one is required |
| `too_large` | A bounded read whose file exceeds `limit`; no partial bytes |
| `locked` | A `lock` whose lease is held by another call; the retry policy is the caller's |
| `cancelled` | The call was cancelled before it changed anything |

Operation refusals are not in this table: they arrive as the tool result's own `isError` with the
code the operation already uses, so one code vocabulary serves the local and the remote path.

## 5. `store`

`store` is the Session state face. It is one name with two operation families and one
notification, provided by the daemon that owns the Session:

| Family | Operations |
| --- | --- |
| Timeline | `append` (sync), `head`, `branches`, `readPage`, `readSegments`, `query`, `trace`, `batchEnd`, `saveSnapshot`, `latestSnapshot` |
| Timeline completion | `commits`, `closeCommits`; notifications `{subscriptionId, batchId, branchId, through, result}` |
| Session rows | `createSession`, `sessionConfig`, `releaseSession`, `rollbackSession`, `associateWorktree`, the row-set operations for sessions, owners, branches, content, snapshots, grants, metadata, settings, providers, agents and Session-file rows, and `fork` and `delete` |

### 5.1 Ordered admission and durable completion

`append` keeps the existing contract, split into data: **admission is synchronous and ordered,
completion is a notification in branch sequence order.**

```text
append {sessionId, branchId, batchId, events}     (sync)
  -> {accepted: true} | error {code, message}
commits {sessionId}                              (async)
  -> {subscriptionId}
notification {subscriptionId, batchId, branchId, through, result: "committed" | {code, message}}
```

- `append` is a **sync** operation, like `localId.get`: it validates, assigns the batch its place
  in the Session's single writer queue, and answers `{accepted}` without waiting for I/O. It is the
  one face operation a caller may issue inside its own commit lock, because it performs no I/O and
  returns nothing the caller must await. The Timeline's existing admission — one
  `TimelineStore::append` under the branch's commit mutex — maps onto it one to one: batches are
  queued in call order, so per-branch sequence order is the admission order.
- `commits` registers the caller's completion listener and answers a subscription id. The
  provider then delivers one notification per accepted batch, after that batch is durable or has
  failed, **in branch sequence order**. `through` is the inclusive sequence of the batch's last
  event on `branchId`. It advances the durable watermark only when `result` is `"committed"`.
- A failed batch is reported with its `batchId` and error, and rejects later `append` calls for
  that Session with a storage error until it is released. The failure is not delivered twice.
- `releaseSession` and `rollbackSession` answer only after every batch the Session had accepted
  has completed or failed: draining is the reply, so a caller that closes a Session knows no write
  is still in flight. A Session's close waits for its release.
- `closeCommits {subscriptionId}` ends a listener; the caller's unload ends it too.
- A remote consumer must not call `append`: the store is provided where the Session state is, and
  a synchronous call across a link would block the caller inside its lock. Reading history
  (`readPage`, `query`, `trace`, and the completion notifications) is the part a remote consumer
  may use.

### 5.2 Session rows

- `associateWorktree {sessionId, repository, root, subtree}` writes the worktree directory record
  and the Session usage association as one commit and answers `{}` after it, so the reply is the
  acknowledgement the file facts owner waits for before publishing a confirmed root (§4.2.1). Its
  deduplication, its place in Session deletion and its format decision belong to
  [worktree access §4](worktree-access.md#4-持久使用关联与清理边界) and the storage owner; the
  directory identity is Session memory and is not stored by this operation.
- `fork` and `delete` keep their accepted storage contract: the visibility switch is one
  transaction, and the copy or removal around it is written in bounded chunks so one large
  Session cannot hold the writer for every other Session
  ([deletion and fork](appserver/session-deletion-and-fork.md),
  [timeline storage](../architecture/rust-agent/timeline-storage.md) §3). This face does not
  restate those steps; it exposes them as one operation each.
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
face, not a second contract (§6). The binding of `append` is the synchronous admission and the
binding of `commits` is the publish-callback registration: the same two things the runtime has
today, with the same ordering and the same draining.

## 6. Provider, binding, and the owners that use each operation

| Old name | Face | What the name is now |
| --- | --- | --- |
| `hostPlatform` | `execHost` | The provider and its Rust binding: `ProcessTree`, `PtyProcessTree`, temporary directories and shell preparation are its internals, reached through the operations |
| `fileStorage` | `fsHost` | The provider and its binding: roots, leases, generations and one validated path component per open are its internals |
| `sqliteStorage` | `store` | The SQL engine inside the provider |
| `timelineStore` | `store` | The Timeline half of the face; the `TimelineStore` trait is its binding |

For the owners below, Rust callers use a thin binding over the operations — a direct
in-process call rather than JSON — and none of them publishes a second API: there is no owner
trait beside the face, no object type another module constructs, and no method set that the face
does not have. A consumer that needs something the face does not offer needs a new operation on
the face, not a new accessor beside it.

The owners that read node files or start node processes, and the operations they use:

| Owner | Operations |
| --- | --- |
| WorkspaceTools file tools | `resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions` |
| SessionStore Session files and command output | `createDirectory`, `replace`, `list`, `remove`, `execHost.removeOutputDirectory` |
| Settings sources and their CAS writes | `readBytes`, `stat`, `lock`, `readBytes {leaseId}`, `replace {leaseId}`, `unlock` |
| MCP auth store, managed OAuth and OAuth token stores | `readBytes`, `lock`, `readBytes {leaseId}`, `backup`, `replace {leaseId}`, `unlock` |
| Hooks project file (`.dimcode/hooks.json`) | `stat`, `readBytes` |
| PromptSources (environment, project and user rule files) | `execHost.info` (`os`, `home`, `timezone`, `execShell`), `stat`, `readBytes` |
| UI extension resources | `stat`, `openRead`, `readAt`, `closeRead` |
| Migration ledger and snapshots | `createDirectory`, `list`, `remove` |
| Hooks configuration commands | `execHost.configured` |
| Worktree association | `store.associateWorktree` |

## 7. Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Each face belongs to the part that owns the resource or the state; consumers stay in their own scope. No new Capability on the control side |
| §4 rule 1 | Command events are not added. A background command's fact is still `exec/started`/`exec/ended` on its branch, written by its owner |
| §4 rule 2 | A handoff keeps its fact on the branch and its process with the provider; nothing is handed to a second owner |
| §4 rule 3 | No before callback and no commit-time I/O. `store.append` is the one operation allowed inside the caller's commit lock because it queues without I/O and does not wait; completion arrives through `commits`, after the commit and outside every lock. Every other face operation is outside the lock |
| §4 rule 4 | Core, assembly and the Timeline carry no remote-execution or engine branch; the provider is chosen by deployment |
| §4 rule 5 | No batch semantics change: `append` admits one atomic batch, and the completion notification carries the same commit and publish boundary the callback does today |
| §4 rule 6 | File content and command output reach the model only through tool results, as they do today |
| §4 rule 7 | Consumers use the faces; they read no provider internals, and the providers use only their own host facilities |
| §4 rule 8 | The faces add no Timeline, Dispatcher or event-format change. One runtime requirement is unmet: [#861](https://github.com/arcships/dim/issues/861), per-call resolution of a Session's process-scope service by its resource node. It is the dim runtime's, solved with rutis's current capability; no requirement is raised to rutis ([overview §6.3](overview.md#63-multiple-nodes), [principles §4 rule 8](../architecture/rust-agent/principles.md)) |
| §4 rule 9 | No new Timeline fact: `exec/started` and `exec/ended` already exist, and the faces write none |
| ADR 002 D2 | No wiring plugin: each face is its provider's own service |
| ADR 002 D3 | Dependencies follow scope: `execHost` and `fsHost` are process names a leaf can provide; `store` is provided where the Session state is |
| ADR 002 D4 | Business configuration is read at use time: the switches at each `start`, the shell preference at each spawn and each environment prompt, the engine at open |
| ADR 002 D5 | §3–§5 are the language-neutral contract; the Rust bindings implement them |
| ADR 003 D1, D2, D5 | No boundary switch in a call; a preset, approval or replay confers none; `configured` runs because of configuration, outside the agent boundary, as hooks already do |
| ADR 009 | Permission decides; `execHost`'s provider enforces |
| ADR 010, ADR 011 | The switches stay the host user's settings, read and enforced by the provider |
| ADR 012, ADR 013 | Grants and Session-file ownership stay control-side records; paths are the node's |
| ADR 014 | The four facts keep their sources: resolution, workspace, system and the worktree probe are the node's, the row facts the control node's. `resolve` never publishes an expansion; the Session part publishes only after the store acknowledged the association and the candidate passed the recheck (§4.2.2) |
| Worktree access R1 | The Session part answers "known range" from its own view before it asks the node to probe; the probe is one extra field on the same call, not a second facts service |
| Worktree access R2 | The directory record and the Session association are one store commit, written before any publication |
| Worktree access R3 | Process preparation resolves its explicit cwd through `resolve` before `start`/`shell`, and hands the confirmed range as `roots`; a running process keeps what it started with |

## 8. Requirements raised to mechanism owners

One runtime requirement, already raised: [#861](https://github.com/arcships/dim/issues/861), the
host side resolving a Session's process-scope service per call by the Session's resource node.
[Overview §6.3](overview.md#63-multiple-nodes) states it as a runtime requirement and explicitly
raises nothing to rutis; this design likewise solves it with rutis's current capability, and an
implementer must not use a gated dependency, which would park a consumer while a leaf is offline
instead of failing one call with `node_unavailable`.

No other requirement is raised:

- The store face keeps the existing Timeline admission and publish ordering; the only change is
  that completion is delivered as a notification on the face instead of a Rust callback, which is
  a binding inside the provider, with no change to sequence, batches, interrupt handling or
  reload.
- Recognition records belong to [worktree access](worktree-access.md) R2 and the storage owner;
  the format decision and the chunked fork/delete steps stay theirs.
- `configured` is the existing process start with an initial stdin expressed as data, and
  `info.execShell` is the existing shell status read. Neither needs a new mechanism.

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
   background policy and the output-path registration are its.
6. **One `store` name or two.** The Timeline family and the Session-row family are one face with
   two families here because one repository port serves both (#1298). If the implementation finds
   two names clearer, the split is a naming decision on that PR, not a new contract.
7. **Root identity form.** The Session passes the opaque `rootIdentity` back to the provider.
   The provider owns its platform form and comparison.
8. **Retained task records.** Settlement is explicit (§3.8). If a long-lived local provider wants a
   bound instead, it must give the control side time to record the terminal fact and refuse a
   repeated `start` until expiry. Registered file retention follows Session ownership (§3.8).

## 10. Ablation

The faces are operations over data with ids, not adapters: `start`/`wait`/`kill`/`output`/`tail`
reuse the existing process tree, capture, page rule and fault vocabulary; `shell`/`write`/`resize`
are the PTY primitives the shell already has; `info`, `confined` and `configured` are the reads
and starts the platform already performs, including the configuration command Hooks runs.
`removeOutputDirectory` carries the existing HostPlatform cleanup across the face. Paging by a
registered output path reuses `output` after the provider releases its task record.
`fsHost` adds no primitive file API and no per-file traversal: its storage family is the reads and
leases the node's storage already offers, needed because a path's facts and its bytes cannot be
read on a machine that does not hold the file; `resolve` is where resolution and the worktree
probe belong. `store` publishes the repository operations the state owner already has, so the SQL
engine and the `TimelineStore` trait stop being second contracts instead of gaining a wrapper;
admission stays synchronous and completion keeps the callback's ordering as a notification.

The `worktree` candidate carries the identity needed to recheck the mapping after the association
commits. `retainedFrom` and `reset` let a caller resume a bounded capture after its cursor expires.
Resolving `cwd` on the node completes recognition before the process starts.
