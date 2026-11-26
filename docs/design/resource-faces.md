# Resource and store services

Status: Draft. Issues: [#1295](https://github.com/arcships/dim/issues/1295) (`hostPlatform` service methods),
[#1296](https://github.com/arcships/dim/issues/1296) (leaf identity and lifecycle),
[#1297](https://github.com/arcships/dim/issues/1297) (`fsHost`),
[#1298](https://github.com/arcships/dim/issues/1298) (`store`); parent
[#1292](https://github.com/arcships/dim/issues/1292). Policy: [service faces](service-faces.md).
Background: [remote plugins §3–§4](research/remote-plugins.md).

This is a design, not an implementation record. Every operation below is a target shape.

A "data face" below means a service's public operation set: the service itself, served by name,
not a separate object. Business code writes no `HostDispatch`; `#[rutis::service]` generates it,
and a service is served in the scope where it is provided.

## 1. Scope

Resource and state operations use these public services:

| Face | Operations | Provided by | Issue |
| --- | --- | --- | --- |
| `hostPlatform` | `info`, `execShell`, `persistentShell`, `sandboxBackend`, `execRoot`, `spawn`, `wait`, `kill`, `settle`, `output`, `retainOutput`, `tail`, `shell`, `write`, `resize`, `spawnWithStdin`, `spawnConfined`, `removeOutputDirectory`, `childPath` | the process owner | #1295 |
| `fsHost` | tool family: `resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions`; storage family: `stat`, `readBytes`, `openRead`, `readAt`, `closeRead`, `lock`, `unlock`, `backup`, `createDirectory`, `replace`, `list`, `remove` | the file-operations owner | #1297 |
| `store` | Session-row repository operations (§5.2) | the process state owner | #1298 |
| `timelineStore` | Timeline operations and completion notifications (§5.1) | the Session state owner | #1298 |


```text
 control node                                        resource leaf (mounted per #1296)
   WorkspaceTools          ── fsHost ─── data ──────►  file operations on node files
   exec, ShellSession      ── existing services ───►  processes, PTY, output files
   settings, auth, Hooks   ── fsHost ─── data ──────►  the same face, answered locally
   Permission and Approval                            never asked, never decides
   Session repository      ── store ───────────────►  the state owner, locally
   Timeline facts          ── timelineStore ───────►  the same state owner, locally
   Worktree associations   ── store ──────────────►  the same state owner, locally
```

**A service is the shape; the mount is a deployment.** `hostPlatform` and `fsHost` are provided by
the daemon that holds the process, the file or the session's own configuration — the control
node's own plugins, or a resource leaf mounted per
[#1296](https://github.com/arcships/dim/issues/1296). `store` and `timelineStore` are provided where the Session state is, always in the daemon that owns
that Session
([#1298](https://github.com/arcships/dim/issues/1298)); it is the same face whether the rows live in
SQLite or PostgreSQL. Whether a face is reachable over a link is decided by deployment, not by a
classification on the name.

The public contract and migration rules are in [service faces](service-faces.md) §2.
For these resource faces, §6 maps `hostPlatform`, `fileStorage`, `sqliteStorage` and `timelineStore`
to their public operations and Rust bindings. A service's public methods are its contract; there
is no separate face object and no `execHost` name.

What stays on the control side, in every case: Session and branch scope, admission
(`Approval`, and `Permission` with the [ADR 014](../adr/014-file-access-facts.md) table), the
`exec/started` and `exec/ended` facts and the wake notice, the task record, Session-file rows,
the worktree Session view, and the recorded tool result. No face asks the user anything, decides
whether a call may run, or consults who is calling.

## 2. Rules for these services

1. **One service, one contract.** The operations are the service's public methods; a Rust typed
   client is a binding over the same methods and owns no second business logic. There is no owner
   trait beside the service.
2. **One call per operation.** `read`, `write`, `edit`, `glob`, `grep` and `mentions` are one call
   each; a traversal is one call, not one call per file.
3. **Paths are node paths.** A request carries a node-absolute resolved path; a reply returns
   node-absolute resolved paths only. A relative path in a request is `invalid`, with one
   exception: **`fsHost.resolve` is the resolution entry point**, so it accepts the path as the
   model gave it — relative to the `cwd` the caller passes, starting with `~`, containing `..`,
   following symlinks — because only the node can resolve it. `cwd`, `workspace` and the
   confirmed roots it is given are node-absolute. Every other operation, the task and shell
   operations included, takes a path a `resolve` reply or the caller's own record already returned.
   Returned paths are stable for history: the same file always resolves to the same path.
4. **Ids are values.** A task, read or lease id is a string the provider issues or the caller
   assigns; ids carry no authority.
5. **Pages are bounded.** Large output and long results cross as pages with a cursor, never as a
   stream. No reply carries unbounded bytes.
6. **Outcomes and failures are different data.** What the operation did is a tool result
   (`content`, `structuredContent`, `isError`, `effects`) and keeps the codes it uses today. A
   call that could not be served is `{code, message}`; the control side returns that code and
   message as the tool result unchanged, so the two vocabularies stay one.
7. **Cancellation is a call.** The caller drops the call or sends the bridge's cancel; a service
   method that needs it takes the existing `cancelled: CancelledFn` parameter (`toolHost.execute`
   is the shape), not a separate `cancel` operation. Cancelling a read of an outcome changes
   nothing; cancelling a process is `kill`.
8. **No per-service authorization.** Authorization is `Approval` and `Permission` over agent
   actions, and connection trust is #1296's. A service carries no credential, preset or grant.
9. **Names are camelCase.** Request, reply and error members are camelCase, as the rest of the
   migration's wire types are.

## 3. Process and task services

There is no separate `execHost` face. A public service is its own contract: `#[rutis::service]`
serves its public `&self` methods in the scope where it is provided, and `xtask types` generates
the TypeScript declarations. A leaf mounts a part copy in the host's instance, so a process,
Session or branch service crosses a leaf without a second face
([research/remote-plugins §4.2](research/remote-plugins.md#42-叶子插件级远程化的单位)).

The process operations are split by their owners:

| Service | Scope | Public operations | Owner document |
| --- | --- | --- | --- |
| `hostPlatform` | process | `info`, `execShell`, `persistentShell`, `sandboxBackend`, `execRoot`, `spawn`, `wait`, `kill`, `settle`, `output`, `retainOutput`, `tail`, `shell`, `write`, `resize`, `spawnWithStdin`, `spawnConfined`, `removeOutputDirectory`, `childPath` | [HostPlatform](../architecture/rust-agent/plugins/HostPlatform.md) |
| `toolHost` | session | `execute` | [tool contributions §2.1](../architecture/rust-agent/tool-contributions.md#21-宿主工具的-toolhost-数据面) |
| `backgroundExec` | branch | `output`, `outputPage`, `wait`, `stop` | [WorkspaceTools data face](../architecture/rust-agent/plugins/WorkspaceTools.md#data-face) |
| `sessionMethods.shell` / `ShellSession` | session, branch | the shell operations | [ShellInput](../architecture/rust-agent/plugins/ShellInput.md) |

`hostPlatform`'s handle methods (`spawn_tree`, `spawn_with_stdin_process`, `spawn_pty`,
`build_process_spec`, `child_path_process`) stay `#[service(skip)]`; they carry process handles and
cannot cross. The service serves the command task operations on `hostPlatform`: `spawn` returns a task id and
the process/output state stays with the owner; `wait`, `kill`, `settle`, `output`, `retainOutput`
and `tail` work by that id. `WorkspaceTools` and `backgroundExec` use the task id; the branch keeps
the task record and the facts. `shell`/`write`/`resize` are served by the same owner; the owner
records the command capture and `ShellSession` keeps only queue, timeout and occupancy policy. `spawnWithStdin` and `spawnConfined` are the existing methods
Hooks and OCR call, now with data requests and replies. `removeOutputDirectory` and `childPath`
are the existing cleanup and PATH helpers, now data methods; SessionStore and MCP call them. The
served `childPath` composes from UTF-8 data; a non-UTF-8 daemon PATH is accepted by the native
`child_path_process` as `OsStr` and is not expressible through the face, so no replacement
characters are written. The
in-process cores stay as the shared implementation, not a second public operation.

The command process, output file, PTY and command capture are owned by `HostPlatform`; the branch
keeps the task record and writes the facts. `shell` writes its bounded capture to `outputPath`, so the path is
real for both task and file reads. `ShellSession` creates the command id and marker pair, sends them
with `write`, and keeps only the queue, timeout and occupancy policy; foreground output, the exit
code and the task log are read back by command id through `output`, `wait` and `settle`. The owner
keeps its line queue private: public reads are terminal records plus `output`, `tail`, `wait` and
`retainOutput`; no public event cursor or shell state exists. Background shell jobs are registered in the owner under
the branch task id; the control side never sees a host pid. `wait` takes the existing cancellation callback; a timeout stops the wait and answers `running`,
and neither a timeout nor a cancelled call kills the task. A command's own `spawn.timeoutMs` still
terminates it at that deadline; stopping a task is an explicit `kill`. `spawnConfined` success means the helper stayed within its limits; an output limit is an
error, so its reply carries no truncation flags. Cancellation is the existing
`cancelled: CancelledFn` parameter (`toolHost.execute` is the shape); it is not a separate
`cancel` operation and carries no `callId`.

### 3.1 `hostPlatform.info`

`info()` answers the process facts consumers read today:

```text
{os: "darwin" | "linux" | "win32", arch: string, home: string, timezone?: string,
 execRoot: string, platformRoots: [string],
 backend: {state: "available"} | {state: "unavailable", reason: string},
 execShell: {state: "available", executable: string, edition: string,
             source: string, outputEncoding: string}
          | {state: "unavailable", code: string, reason: string}}
```

`execShell` is the existing `ShellStatus` shape; `backend` is `SandboxBackend`; `execRoot` is
the platform's execution root. `execShell`, `sandboxBackend` and `execRoot` remain available as
individual methods for a caller that needs one fact; `info` is the one call for a caller that
needs several.

### 3.2 Settlement and retained task records

A task id is reserved by the owner before the command or job starts. `spawn` and the `job` part of
`shell.write` reject an id already in the live table with `task_exists`; a completed command keeps
its output log, so reusing its id is refused until the log is removed. `spawn` also refuses an
unusable output directory before the process starts.

`wait` reports the terminal record but never removes it; a timeout answers `running` and only
stops waiting. The caller decides when the record is no longer needed and calls `settle`. `settle`
accepts only a terminal task or a shell that exited; a running command, shell or background job
answers `task_running`; settling an already released id is `Ok`. `kill` is the explicit stop; a second kill while a stop is pending answers `task_running`; when
it ends a background job the owner marks that job ended so it can be settled. `wait`, `kill`,
`output` and `tail` address shell command and job ids as well as exec commands, and `output` for a
job performs a bounded seek/read of its log. A terminal shell answers `write`/`resize` with
`task_ended`. A caller that never settles keeps the record and its log until the owner unloads.

Command and job output logs live below the output directory the caller gives the owner; the
persistent shell defaults to `execRoot`. The branch registers the logs it gives the model (foreground,
occupying, background and shell); Session deletion removes the registered candidates. A shell log
is registered before the `shell` reply exposes it and is kept until `removeOutputDirectory` or
Session deletion; settling the shell does not delete it. The init log is internal and is removed
when its command is released. `retainOutput` is idempotent. `removeOutputDirectory` accepts only a
UTF-8 path that is a persistent-shell output directory strictly below the canonical `execRoot`;
`.`/`..` and every symlink component are refused, an absent target is `Ok`.

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
| `remove` | async | `{path}` / `{parent, name}` | `{}` |

A listed name passes unchanged to `remove({parent, name})`: a Unicode string, `{Unix: number[]}` for native bytes, or `{Windows: number[]}` for native UTF-16 code units. The owner checks that the name is a single component.

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
  or the caller's connection. `readAt` retains the existing 64 KiB maximum per
  call; a larger requested limit is clamped, zero or an offset at/past the pinned
  length returns empty bytes. UI resource streaming uses these bounded reads. Plugin-file
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
| Worktree directory record and Session usage association, deduplicated and written atomically | `store.associateWorktree`, provided by SessionStore (worktree access §4, R2) |
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
through `store.associateWorktree` and the store's own Session deletion step, never by reading
tables: no other part queries them for admission.

#### 4.2.2 Revalidation before publication

The candidate is a filesystem observation made before a storage wait. It must still be true when
it is published, or it must not be published:

1. After `store.associateWorktree` returns, the Session part checks its own lifecycle: a Session
   that is closing, deleted or between generations publishes nothing and answers the call with its normal
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
and is read through the branch's `backgroundExec.output` / `backgroundExec.outputPage`.

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

## 5. `store` and `timelineStore`

The repository and Timeline operations use two names on the daemon that owns the state: process
`store` provides the repository operations (§5.2), retaining its existing Timeline `query`,
and Session-scoped `timelineStore` provides the Timeline operations and completion notifications
below. Both use the existing state owner; the split follows §9(6), without changing Session
placement or the storage engine.
SessionTitle and FileAccess use the process repository operations in the [resource owner contract](contracts/resource-owner-operations.md).

| Family | Operations |
| --- | --- |
| Timeline (`timelineStore`) | `createSession`, `sessionConfig`, `releaseSession`, `rollbackSession`, `append` (sync), `head`, `branches`, `readPage`, `readSegments`, `query`, `trace`, `batchEnd`, `batchEndAt`, `saveSnapshot`, `latestSnapshot` |
| Timeline completion (`timelineStore`) | `commits`, `closeCommits`; notifications `{subscriptionId, batchId, branchId, through, result}` |
| Session rows (`store`) | The repository operations listed in §5.2 |
| Worktree association (`store`) | `associateWorktree`; [service faces](service-faces.md) §4.8 |

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

#### Binding shapes and lifecycle

The Timeline methods take one request object. Keys are `{sessionId, branchId}`;
`head` returns `{sequence}`, `branches {sessionId}` returns `{branches:[{info,sequence}]}`.
`readPage {sessionId,branchId,after,limit,through}` returns `{events}`. `limit` is a target event
count: the final complete atomic batch may exceed it. `readSegments {sessionId,segments:
[{branchId,from,through}]}` returns `{segments}` in request order, using the existing shared read
scope. Each stored record is `{event,trace,batchEnd}` using the canonical event and trace data.
The Rust `TimelineStore::read(key, range)` convenience corresponds to one interval of
`readSegments`, returning that segment directly. The native binding preserves backend overrides;
it does not introduce a separate wire operation or replace an override with fallback probes.
`batchEnd {sessionId,branchId,sequence}` validates the boundary and returns `{}`;
`batchEndAt` returns `{sequence}` for the containing batch's end through the backend's existing
operation, without repeated remote probes. `trace` takes the same position and returns `{trace}`.
`query` takes the scope, query and optional page described in §5.2 and keeps that canonical reply.

`createSession {sessionId,config}`, `releaseSession {sessionId}`, `rollbackSession {sessionId}`
and `saveSnapshot {sessionId,branchId,snapshot}` return `{}`. `sessionConfig {sessionId}` returns
`{config}`. `latestSnapshot {sessionId,branchId,through,interpreter}` returns `{snapshot}`.
These are storage lifecycle steps. `createSession` prepares the backend record; it does not
initialize or publish an Agent Session. `rollbackSession` undoes a successfully prepared creation
whose initialization failed, after its producers have stopped; it is not the general deletion
workflow. `releaseSession` drains accepted backend writes and releases their resources without
closing the Agent Session or its admission. App Server creation/deletion retain the repository
and lifecycle obligations in §5.2.

Snapshots retain their canonical fields (`sequence`, `event_id`, `interpreter`, `state`), optional
acceleration semantics and owner validation. Domain errors preserve `{code,message}` using
`invalid`, `closed`, `cancelled` or `storage`; parsing rejects malformed requests at the boundary.

The bridge binding of `commits` is `commits({sessionId}, listener) -> {subscriptionId}`. The caller
must declare `timelineStore`; the owner registers the listener with that calling row before
returning, and the row's unload withdraws it. Register before the batches to observe are admitted;
there is no replay of earlier admissions. `closeCommits` withdraws it explicitly and is
idempotent. Each listener receives only its requested Session's completions, tagged with its own
subscription id. A slow or failed observer does not delay durable completion or release, and
ending observation never cancels a write or the native publication callback. The calling row must
belong to the runtime in which the provider installed the binding; sharing the native binding
with another runtime does not connect that runtime's listeners to the provider's completion bus.

The native binding retains synchronous `append` admission and its existing publish callback.
Its notification `batchId` is the JSON encoding of `[branchId, through]`, the batch identity
already known at admission; local data calls preserve their supplied `batchId`. This identifier
is notification correlation only, with no additional durable fact or lookup. A synchronous
admission refusal produces no completion notification. The one backend callback keeps native
publication synchronous, then enqueues observation on rutis's existing ordered asynchronous
bus. Release drains that backend's writes and callbacks; it does not wait for observers to finish.
Foreign-language rows in the provider runtime may observe and read; a separate runtime has
read-only access. Synchronous append across a link is rejected as stated in §5.1.

The Rust `TimelineStore` trait is the local binding of `timelineStore`, not a second contract
(§6); the SQL engine remains the provider's implementation. The binding of `append` is
synchronous admission, and the binding of `commits` is publish-callback registration,
with the existing ordering and draining.

### 5.2 Session rows

The process `store` face exposes the existing Session repository operations below.
Every method is asynchronous. Paths are the caller's already resolved absolute strings;
`config` is the existing SessionConfig data and `configText` retains its exact stored bytes.
The operations keep the owner transactions and existing existence checks; they do not
resolve paths, decide permission or create a second Session lifecycle.

| Operation | Request | Reply |
| --- | --- | --- |
| `addGrant` | `{sessionId,path,kind,mode}` | `{}` |
| `removeGrant` | `{sessionId,path}` | `{}` |
| `grants` | `{sessionId}` | `{grants:[{path,kind,mode}]}` |
| `setSessionLanguage` | `{sessionId,language}` | `{}` |
| `creationRecord` | `{sessionId}` | `{origin,leaderSessionId}` |
| `owner` | `{sessionId}` | `{agentId}` |
| `hasRecords` | `{sessionId}` | `{exists}` |
| `createOwnedSession` | `{sessionId,config,agentId,protocolVersion,leaderSessionId,origin,worktree}` | `{}` |
| `sessionConfig` | `{sessionId}` | `{config}` |
| `sessionConfigText` | `{sessionId}` | `{configText}` |
| `fork` | `{sourceSessionId,sessionId,at,configText}` | `{}` |
| `delete` | `{sessionId}` | `{removed}` |
| `worktreeDirectories` | `{}` | `{directories:[{repository,root,lastUsedAtMs}]}` |
| `sessionsUnderWorkspace` | `{root}` | `{sessionIds}` |
| `worktreeUsages` | `{repository,root}` | `{usages:[{sessionId,subtree}]}` |
| `removeWorktreeDirectory` | `{repository,root}` | `{removed}` |
| `removeWorktree` | `{repository,root}`, asynchronous removal callback | `{removed}` |
| `removeUnregisteredDirectory` | `{root}`, asynchronous removal callback | `{removed}` |
| `catalog` | `{request,agent?,version,hosts,closed}` | existing catalog page |
| `query` | `{scope,query,page?}` | `{result,next}` |

These are repository steps, not the client lifecycle workflows `sessions.delete` and
`session.fork`. Trusted plugins may address any Session id; the process face does not apply
per-caller Session ownership or grant scoping. The lifecycle owner remains responsible for:

- `createOwnedSession`: reserve the creation/admission identity, select a supported protocol,
  validate the configuration and apply that protocol's live feature restrictions before
  persisting it, then initialize/publish the Session or roll the preparation back.
- `delete`: close the Session and settle its producers before removing its rows and output.
  This operation neither closes a live Session nor removes its Agent admission slot.
- `fork`: validate the source collaboration role, published main cut, atomic/context boundary
  and rest point; supply the source's stored configuration text with `initialTitle` removed,
  then initialize the fork under the source Agent/protocol. The repository copies the supplied
  text without applying live protocol exclusions or rebuilding the configuration.

`createOwnedSession`, `fork` and `delete` reject Session ids that are not a single normal path
component with `invalid`, before acquiring ownership or touching rows or output paths. These
checks are shared with the native Session admission/deletion boundary.

Grant kind is `file` or `dir`, mode is `read` or `write`; adding read never downgrades
an existing write grant. `language` is a ReplyLanguage code or null to clear it.
`origin` is the existing Session origin. `leaderSessionId`, `agentId`, `config` and
`configText` are null when the corresponding optional owner value is absent;
`creationRecord` and language writes still reject a missing Session. `worktree`
is null or `{repository,root,subtree}`. Session creation retains the atomic
owner/configuration/worktree publication. `lastUsedAtMs` is null for legacy directory
records without an association timestamp.

`removeWorktree` holds the owner exclusion for its usage checks and for registering the
resolved root as being removed, then releases it and awaits the callback, which receives
the checked root path; the exclusion is taken again only to delete the directory row after
the callback returns. No SQL or Timeline commit lock is held while awaiting it. The
registration ends in the removal's own task, so dropping the caller stops neither the
deletion nor the registration. While it runs, `createOwnedSession`, `fork`,
`store.associateWorktree`, `removeWorktreeDirectory`, `removeWorktree` and
`removeUnregisteredDirectory` are refused with `invalid` for a path at, under or above the
registered one, and proceed for unrelated paths. The callback may refuse with `invalid`,
`cancelled` or `storage`; unexpected callback or transport failures are `storage`. A
callback failure leaves the directory row in place and no registration. The registration
is the only record of the removal: there is no separate check/remove lease, and a Session
creation on a directory being removed is refused instead of waiting for it.
`removeWorktreeDirectory` is the existing database-only candidate cleanup, with its
existing association checks and the same refusal for a path being removed.

`removeUnregisteredDirectory` takes `{root}` and the same asynchronous removal callback.
It is the unregistered removal of
[worktree management §5](worktree-management.md#5-列出删除与清理): the root is absolute
without relative components, and the operation holds the owner exclusion for its
directory-record and workspace checks and for registering the resolved root, then awaits
the callback, which re-checks the unregistered filesystem facts before it deletes
anything. A callback failure is `invalid`, `cancelled` or `storage` as above. The
registration keeps the root refused to the operations above until the same removal task
clears it, and the deletion itself has no time limit: it covers a recursive `--force`
deletion of any size. Dropping the caller stops neither the deletion nor the registration.

`catalog` takes the existing Session catalog request and the calling Agent owner's
loaded/closed snapshots. Its result is the stored catalog page: sequence comes from the
database and `closed` is false on each returned row. The Agent lifecycle owner enriches that
page with live sequence and closed state for client responses; `store.catalog` does not read
or reproduce AgentIndex. Its pagination and filtering remain the storage catalog contract. `query` takes the existing fixed read scope: `{key:{sessionId,
branchId},through,segments:[{branchId,from,through,storedThrough,tail}]}`. Its query,
page, result and immutable tail events retain the Timeline query contract. Neither
operation adds a preliminary query or rereads the caller's snapshots.

Refusals are `{code,message}`: `invalid`, `closed`, `cancelled` or `storage`, preserving
the owner message. Existing absence behavior is retained: `addGrant` on an unknown Session
fails `storage` (foreign-key refusal), `removeGrant` succeeds without a matching row, and
`setSessionLanguage` fails `closed`. Unsupported protocol versions retain the native database
`storage` refusal; the lifecycle owner validates versions before preparation. These methods expose storage operations to trusted plugins, not
new external client entry points. Session files use `sessionFiles`. Metadata consumers call `store.readMetadata` and `store.setTitle`; FileAccess calls `store.associateWorktree`. Retire the catalog entries `sessionMetadata` and `worktreeAssociations` when migrating their callers.

The Rust `Store` binding exposes only these repository operations and directly returns
the same owner's operation futures. Its typed key and dispatcher share one binding;
`SessionStore` remains the native bootstrap/implementation owner, not the public `store`
type. Title reads use `store.readMetadata`. Native shared result types
and filesystem paths retain their existing lossless Rust representation; only the wire
boundary serializes paths as data. The existing asynchronous removal callback keeps its
original path argument and ownership exclusion.


- `store.associateWorktree {sessionId, repository, root, subtree}` writes the worktree directory
  record and the Session usage association as one commit and answers `{}` after it, so the reply is the
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

## 6. Provider, binding, and the owners that use each operation

| Old name | Face | What the name is now |
| --- | --- | --- |
| `hostPlatform` | `hostPlatform` | The process service and its Rust binding; its public data methods serve `info`, the command task operations, the persistent shell, the configured/confined helpers, removal and child path (the list is in §3). `ProcessTree`, `PtyProcessTree`, temporary directories, shell preparation and the `_process` cores are internals (`#[service(skip)]`) |
| `fileStorage` | `fsHost` | The provider and its binding: roots, leases, generations and one validated path component per open are its internals |
| `sqliteStorage` | `sqliteStorage.storageReport({})` | SQLite file, WAL and reclaimable bytes ([native storage](appserver/native-storage-format.md)). Session repository operations belong to `store`. |
| `timelineStore` | `timelineStore` | Session-scoped Timeline operations; the `TimelineStore` trait is its native binding |

For the owners below, Rust callers use a thin binding over the operations — a direct
in-process call rather than JSON — and none of them publishes a second API: there is no owner
trait beside the service, no object type another module constructs, and no method set that the
service does not have. A consumer that needs something the service does not offer needs a new
operation on the service, not a new accessor beside it.

The owners that read node files or start node processes, and the operations they use:

| Owner | Operations |
| --- | --- |
| WorkspaceTools file tools | `resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions` |
| SessionStore Session files and command output | `createDirectory`, `replace`, `list`, `remove`; shell output removal stays the Rust `remove_output_directory` call |
| Settings sources and their CAS writes | `readBytes`, `stat`, `lock`, `readBytes {leaseId}`, `replace {leaseId}`, `unlock` |
| MCP auth store, managed OAuth and OAuth token stores | `readBytes`, `lock`, `readBytes {leaseId}`, `backup`, `replace {leaseId}`, `unlock` |
| Hooks project file (`.dimcode/hooks.json`) | `stat`, `readBytes` |
| PromptSources (environment, project and user rule files) | `hostPlatform.info` (`os`, `home`, `timezone`, `execShell`), `stat`, `readBytes` |
| UI extension resources | `stat`, `openRead`, `readAt`, `closeRead` |
| Migration ledger and snapshots | `createDirectory`, `list`, `remove` |
| Hooks configuration commands | Hooks' own process start (Rust); `hostPlatform.configured` only if a foreign caller needs it |
| Worktree association | `store.associateWorktree` |

## 7. Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Each service belongs to the part that owns the resource or the state; consumers stay in their own scope. No new Capability on the control side |
| §4 rule 1 | Command events are not added. A background command's fact is still `exec/started`/`exec/ended` on its branch, written by its owner |
| §4 rule 2 | A handoff keeps its fact on the branch and its process with the provider; nothing is handed to a second owner |
| §4 rule 3 | No before callback and no commit-time I/O. `timelineStore.append` is the one operation allowed inside the caller's commit lock because it queues without I/O and does not wait; completion arrives through `commits`, after the commit and outside every lock. Every other face operation is outside the lock |
| §4 rule 4 | Core, assembly and the Timeline carry no remote-execution or engine branch; the provider is chosen by deployment |
| §4 rule 5 | No batch semantics change: `append` admits one atomic batch, and the completion notification carries the same commit and publish boundary the callback does today |
| §4 rule 6 | File content and command output reach the model only through tool results, as they do today |
| §4 rule 7 | Consumers use the faces; they read no provider internals, and the providers use only their own host facilities |
| §4 rule 8 | The faces add no Timeline, Dispatcher or event-format change. One runtime requirement is unmet: [#861](https://github.com/arcships/dim/issues/861), per-call resolution of a Session's process-scope service by its resource node. It is the dim runtime's, solved with rutis's current capability; no requirement is raised to rutis ([overview §6.3](overview.md#63-multiple-nodes), [principles §4 rule 8](../architecture/rust-agent/principles.md)) |
| §4 rule 9 | No new Timeline fact: `exec/started` and `exec/ended` already exist, and the faces write none |
| ADR 002 D2 | No wiring plugin: each face is its provider's own service |
| ADR 002 D3 | Dependencies follow scope: `hostPlatform` and `fsHost` are process names a leaf can provide; the task and shell services stay in their Session/branch scope |
| ADR 002 D4 | Business configuration is read at use time: the switches at each `start`, the shell preference at each spawn and each environment prompt, the engine at open |
| ADR 002 D5 | §3–§5 are the language-neutral contract; the Rust bindings implement them |
| ADR 003 D1, D2, D5 | No boundary switch in a call; a preset, approval or replay confers none; `configured` runs because of configuration, outside the agent boundary, as hooks already do |
| ADR 009 | Permission decides; `hostPlatform`'s provider enforces |
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

- The `timelineStore` face keeps the existing Timeline admission and publish ordering; the only change is
  that completion is delivered as a notification on the face instead of a Rust callback, which is
  a binding inside the provider, with no change to sequence, batches, interrupt handling or
  reload.
- Recognition records belong to [worktree access](worktree-access.md) R2 and the storage owner;
  the format decision and the chunked fork/delete steps stay theirs.
- `configured` is the existing process start with an initial stdin expressed as data, and
  `info.execShell` is the existing shell status read. Neither needs a new mechanism.

## 9. Open decisions

1. **Naming and instance per leaf.** Process-scope names are global, so `hostPlatform` and
   `fsHost` need one instance per leaf: `hostPlatform.<leaf>` or a host-side route.
   [#1296](https://github.com/arcships/dim/issues/1296) decides; this document assumes only that a
   Session's calls reach one provider.
2. **Disconnect policy.** The node ends the processes it started, per overview §6.3. #1296 states
   and tests the rule for mounted resource leaves; the control-side settlement
   (`node_unavailable`, no restart) follows from it.
3. **`owned` on the control side.** The fact is a row lookup, so it is answered by `sessionFiles`
   with the node's resolved path rather than on the node. If review wants it computed with the
   other three, the rows would have to move, which is #1298's kind of decision.
4. **Where the persistent-shell protocol runs.** The owner records the command capture and
   markers; the control side keeps the queue, timeout and occupancy policy and reads the command
   by id. The owner line queue is private to `ShellSession`, not a public contract. #1295 wires the
   PTY and the foreground/occupancy path.
5. **Result text ownership.** File operations return their tool result from the provider because
   the text is the file operation's own (line numbers, previews, image blocks). A command returns
   records, and the control side composes the tool result, because the task identity, the
   background policy and the output-path registration are its.
6. **Repository face names.** `store` names the Session-row repository operations, and
   `timelineStore` names the Timeline operations and completion notifications. Both delegate
   to the same state owner (#1298); the naming split changes neither placement nor ordering.
7. **Root identity form.** The Session passes the opaque `rootIdentity` back to the provider.
   The provider owns its platform form and comparison.
8. **Retained task records.** Settlement is explicit (§3.2). If a long-lived local provider wants a
   bound instead, it must give the control side time to record the terminal fact and refuse a
   repeated `start` until expiry. Registered file retention follows Session ownership (§3.2).

## 10. Ablation

The removed `execHost` face added no operation of its own. Each operation lives on the service
that already owns it: `hostPlatform.info` (and its `execShell`/`sandboxBackend`/`execRoot`
methods) reads the platform facts the process owner already has; `toolHost.execute` and
`backgroundExec.output`/`outputPage`/`wait`/`stop` are the existing tool and task operations;
`sessionMethods.shell` keeps the PTY primitives the shell already has. No process or PTY state
moves between services, and no handle crosses. `fsHost` adds no primitive file API and no
per-file traversal: its storage family is the reads and
leases the node's storage already offers, needed because a path's facts and its bytes cannot be
read on a machine that does not hold the file; `resolve` is where resolution and the worktree
probe belong. `store` publishes the repository operations the state owner already has;
`timelineStore` publishes its Timeline operations and completion notifications. StorageMethods reads database and WAL sizes through the native binding of `sqliteStorage.storageReport({})`; plugins can dispatch the same operation. SQLite owns the connections and SQL. `TimelineStore` is the native binding of `timelineStore`.
Admission stays synchronous and completion keeps the callback's ordering as a notification.

The `worktree` candidate carries the identity needed to recheck the mapping after the association
commits. `retainedFrom` and `reset` let a caller resume a bounded capture after its cursor expires.
Resolving `cwd` on the node completes recognition before the process starts.
