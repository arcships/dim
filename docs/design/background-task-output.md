# Background command output in the CLI

Status: native method implemented; the CLI detail and refresh interaction remain
to be implemented on `rust-tui`. Scope: the
live command-output part of
[#264](https://github.com/arcships/dim/issues/264). Rules:
[principles §4](../architecture/rust-agent/principles.md#4-通过时间线扩展行为),
[ADR 002](../adr/002-plugin-scope.md), and
[C1 Session methods](contracts/session-boundary.md).

## 1. Problem and existing behavior

The `/tasks` panel on `rust-tui` lists background commands and stops one through
`session.exec.stop`. It cannot display a command's output while the command runs.
The UI is not on `main`; its current implementation is tracked in #264.

The output already exists. Before transferring a command, `ExecProcess` creates
its output file; the pipe pumps keep updating that file and its bounded preview.
`exec/started` carries the task identity and output path, and `exec/ended` carries
the final result. The command's owner is
[BackgroundExec](../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec), not Rpc
or the client.

Existing alternatives do not supply a client read:

- Timeline observation supplies start/end facts, without live output chunks.
- `wait` waits for a notice; it does not read the running process.
- The `read` tool can read an output file, but no client method invokes that
  tool. Reading a daemon path from the CLI assumes both run on the same machine.
- Session files hold immutable stored attachments, not this active output file.

Expose the existing preview through the existing WorkspaceTools Session part.
No output event stream, file-read endpoint, new Capability or core mechanism is
needed. This contract settles paged reading of a task's registered output file;
it does not settle member controls (#263), recently ended task retention,
export, or Workflow task presentation.

## 2. Client method

WorkspaceTools registers the leaf namespace **`session.exec.output`** with the
Session's `sessionMethods` from its `apply`; it declares the tail method of the
same name below and the sibling `session.exec.output.page` after it:

```json
{
  "name": "session.exec.output",
  "mutating": false,
  "interruption": "report",
  "progress": false
}
```

Request: `{sessionId: string, branchId: string, taskId: string}`. All fields are
required and non-empty; unknown fields are rejected at the method boundary.
The native params DTO declares all three fields, including `sessionId` used by
Rpc routing, with `rename_all = "camelCase"` and `deny_unknown_fields`.
Reply: `{text: string}`. An empty string means the active command has emitted
no preview text. It is not a substitute for an unknown task or failed read.

The response uses exec's existing sanitized tail-preview rule: the last 2,000
lines within its 50 KiB budget, including its existing terminal-newline behavior.
It is a snapshot of captured output, not the entire file or a promise that the
process has flushed its own buffers. The UI labels it as a tail preview.
The tail reply takes no caller-supplied path, byte range, cursor, size limit or
subscription.

The leaf namespace matters: the Session's method table refuses, at
registration, a namespace that contains an exact built-in. `session.exec` would
contain `session.exec.stop`; the sibling `session.exec.output` does not.
WorkspaceTools registers the leaf; Rpc's built-in list and namespace checks are
unchanged.

Each call resolves the named branch through WorkspaceTools' existing
`TreeCommands`, then asks that branch's BackgroundExec for the active task's
capture. No Session part retains branch business state.

**Task identity.** Every request resolves `taskId` inside the branch it names:
`sessionId` admits the Session, `branchId` selects that Session's branch, and
the task must be one that branch started. The built-in `exec` mints
`exec-<uuid>` per call, which does not repeat within a Session in practice, but
no owner guarantees a Session-unique id; the contract key is `(sessionId,
branchId, taskId)`, and a task id this branch did not start is
`exec_task_unknown` even when another branch or Session has it. The same key
holds for `session.exec.stop` and `session.exec.output.page`.

Command completion remains authoritative through `exec/ended`. Its
`structuredContent` carries the final `preview` and `previewTruncated` beside
`outputBytes`, `outputTruncated` and `outputPath`: `preview` is the same
sanitized tail the live read shows (the last 2,000 lines within 50 KiB, with
the existing terminal-newline behavior), and `previewTruncated` says those
bounds left something out — bytes outside the captured tail buffer, or a
sanitized tail the rule cut. Both are computed from the same settled capture as
the result's model-readable `Preview:` section; a command that printed
nothing carries `preview: ""`. A recovered end — a task whose
owner restarted — carries neither member, because its final output is unknown
and its file stays readable through `session.exec.output.page`. The members
are additive `structuredContent` fields: no event, method, feature entry or
fixture changes.

| Condition | Result |
| --- | --- |
| Active task, including no output yet | Current preview in `text` |
| Task unknown, no longer active, or only present in replay | Existing `exec_task_unknown` application error, with message `No live output is available for this task`; never open a path from the request or historical events |
| WorkspaceTools is not installed, such as the shell-only assembly | Existing `unknown_method`; no substitute file read |
| Branch/provider unavailable or unloading | Existing branch/C1 availability and interruption errors |
| Malformed request on an available Session | Existing invalid-params error |
| Empty or unknown `sessionId` | Existing C1 routing `session_closed` error, before the capability handler |

Command completion remains authoritative through `exec/ended`. A final read can
race completion: it may return a last preview or `exec_task_unknown`. Neither
response creates a terminal fact. If a listed task returns `exec_task_unknown`,
the UI keeps its last preview, shows `Live output unavailable`, and stops
refreshing without marking the task failed or ended. After receiving the real
terminal event, it displays the existing final result. A read failure does not
stop the command or change its task status.

### Sibling method: the paged full output

Alongside the tail, the same `session.exec.output` namespace declares
`session.exec.output.page {sessionId, branchId, taskId, cursor?, limit?}`, which
reads the task's registered output file (`exec/started.outputPath`) by byte
cursor and answers `{text, bytes, nextCursor?, truncated}`. A page is bounded:
`limit` rejects 0, defaults to 1 MiB, and clamps to 4 MiB; `text` is sanitized
exactly like the preview; and a page ends on a character boundary, so a
character the limit splits is served whole — at the start of the next page when
the cut falls after other characters, and in the page itself when the budget
cuts the very first character, so a page carries at most `limit` plus three
bytes and consumes at least one character while the file has one.

A client walks the file by served bytes, not by `nextCursor`: the next offset is
always `cursor + bytes`, where `bytes` is the file bytes this page consumed
(sanitization can make `text` shorter). `nextCursor` is present only when the
read found at least `limit` bytes — the page was full — and it equals that same
offset, so the client may continue immediately; a shorter page carries no
`nextCursor` even while the task is still writing, and that is not the end of
the walk. `bytes == 0` means the cursor is caught up or ended: the file has
ended at or before it, or a running task has not written the bytes at the cursor
yet, and the client retries the same cursor later. A client-supplied cursor
inside a character answers the same empty page; a cursor the method handed out
never sits inside one. The file is append-only, so a cursor the method handed
out always names a byte that never changes, and rereading it returns the same
bytes.

`truncated` is `exec/ended`'s
`outputTruncated`, the live capture's flag while the task runs, and the file's
size at the 64 MiB cap only for a task whose owner restarted (no flag exists
there). The method opens the path the branch's own `exec/started` fact
registers — never a client-supplied path — so a task id from another branch or
Session cannot select a file; a task whose file was cleaned answers
`exec_output_unavailable`, distinct from the tail's `exec_task_unknown`, and an
unknown task answers `exec_task_unknown` saying no such task is known on the
branch, since an ended task's file is still readable there. A copied Session resolves these facts across its copied prefix; the fork boundary restricts recovery and reposting only, not historical output reads. A fork is admitted only at a rest point, so a copied background task is already terminal; stopping it returns that terminal and never cancels the source Session's process. The
`session.exec.output` tail keeps its preview rule and its
`exec_task_unknown` once the live capture is cleared. This paged read is the
full-file browsing §1 settles; it does not settle a daemon-hosted open/export
method, retention, or member controls.

## 3. Ownership and lifecycle

The active capture is an existing I/O resource. BackgroundExec's task record
retains an optional handle to that same capture; present means live capture,
absent means unavailable.
The table also retains ended/replayed tasks, so table membership does not imply
live output. The handle is released after the supervisor's terminal append attempt
settles, including on write failure; do not release it between process settlement
and that append. The read must not extend the existing resource cleanup lifetime.
The tail read uses the capture's existing synchronization, with no new lock or
second output buffer. The page's byte read takes no capture lock: it opens the
registered file itself and reads from the cursor, so it never waits on the
pump's write. A running task's page does take that same lock once per call, the
one the tail read already uses, only to copy the capture's `truncated` flag —
held for the copy, never across the file read — so a page can hold the pump up
only for that copy. That is safe because the file is append-only — a page only
reads bytes at or past its cursor — and a task is registered only after its
file exists (`ensure_file` runs before `exec/started`), so a known task's
`exec/started` always names its own file; a concurrent write can only extend
the file past the cursor, and a file removed later answers
`exec_output_unavailable`. Release the task-table guard before awaiting either
read.

Register the method on WorkspaceTools' original context after its cleanup is
registered. C1 owns method admission and unload settlement. The read is finite,
starts no background work, acquires no enduring Session hold, and writes no
Timeline event. Process-tree termination, Session holds, end publication and wake
delivery keep BackgroundExec's existing order. Reload/replay never restarts a
process or reconstructs a live capture from its saved path.

## 4. CLI interaction

Extend the existing tasks list and panel machinery on `rust-tui`; do not create
another task tracker. Enter currently stops a command; this interaction changes
to Enter opening its detail and `x` invoking the existing stop method. Keep member
rows on their existing path until #263 specifies theirs.

The current TaskView has no branch, and command stop hard-codes `main`. Preserve
the originating `branchId` while folding events and carry it with the task in that
same projection; both output reads and command stops must use it. This is CLI
implementation work, not a field already available in TaskView or a new event.

```text
+-- Background tasks -------------------------------------------+
| > command  cargo build                              running    |
|   agent    inspect parser                           running    |
| Enter output   x stop command   Esc close                      |
+---------------------------------------------------------------+

+-- cargo build ------------------------------------------------+
| Tail preview - last 2,000 lines / 50 KiB                       |
| Compiling dim-app-server                                      |
| ...                                                           |
| x stop command   Esc back                                     |
+---------------------------------------------------------------+
```

While this detail is visible and the task is active, read immediately, then
refresh once per second after the preceding request settles. Allow only one
request at a time. Closing the detail stops refreshes; an already-admitted read
settles through C1. The UI does not automatically retry a method-unavailable
error. The user can reopen the detail after the service becomes available.

Loading shows `Loading output...`; an empty reply shows `No output yet`.
`exec_task_unknown` follows the non-terminal unavailable behavior above; an absent
method shows `Live output unavailable in this profile`. Other read errors leave
the last preview visible with an error line. An ended task shows
its terminal status and existing final result. Esc returns to the list without
stopping work. At narrow widths, wrap text within the existing overlay and keep
the status/key hints visible; below the CLI's minimum size use its current resize
screen. Render output as text through the existing terminal renderer and reuse
the CLI's existing localization and theme paths.

## 5. Implementation and verification

| Owner | Change |
| --- | --- |
| WorkspaceTools / BackgroundExec | Finite active-capture read, C1 declaration on the existing Session part and leaf namespace registration; no new plugin |
| WorkspaceTools / exec | The background `exec/ended` result gains `preview` and `previewTruncated` in `structuredContent`, computed by the shared bounded-tail rule |
| Rpc | Existing C1 routing and Session admission only; no custom handler |
| CLI | Existing tasks projection, command detail and refresh lifetime; stop remains `session.exec.stop` |

Verify one real App Server flow: start a
background command, hold it after its first output, read that output before
`exec/ended`, release or stop it, and observe the original final result. Include
an empty preview, oversized/terminal-control output under exec's existing tail
rule, wrong-branch/task selection, no capture on replay, absent WorkspaceTools
and provider cleanup. Hold the terminal append and verify the capture remains
readable until that attempt settles. Check that reads do not
introduce events, wakes or extra Session holds.

The CLI flow uses its mock daemon to check list -> detail -> refreshed output ->
terminal result, the changed Enter/x/Esc behavior, one outstanding request and no
refresh after close. Update the current task-panel command-stop flow for the
new Enter behavior, cover a command from a non-main branch through both read and
stop, and exercise unknown capture before the terminal notice without fabricating
an end or failure. Existing transport/CLI flow tests should carry these assertions;
do not add private-helper or mock-order tests. Run `xtask check` and regenerate
graphs when the implementation changes the actual provided service. This
native method does not complete the CLI interaction or all of #264.

## 6. Rule check and ablation

| Rule | Application |
| --- | --- |
| §1 / ADR 002 D1 | The existing Session entry routes each call to the branch that owns the command; no process-level state |
| §4.1 commands/results | Read-only; stop continues through the existing owner method |
| §4.2 handover | Foreground/background transfer and terminal settlement remain unchanged |
| §4.3 before | No before handler |
| §4.4 core neutrality | Rpc uses C1; core has no command-output special case |
| §4.5 batches | No Timeline writes or batch change |
| §4.6 context | UI output is not model context; tool results retain their existing route |
| §4.7 public access | Only WorkspaceTools' own branch service and existing public methods; CLI reads no plugin internals |
| §4.8 infrastructure | Existing C1 admission, namespace checks, facade and lifecycle; no extension |
| §4.9 necessary facts | Existing start/end events suffice; live output creates no durable fact |
| ADR 002 D2 | No adapter, aggregator, separate registry or new Capability |
| ADR 002 D3 | WorkspaceTools uses its existing enclosing dependencies; Rpc does not depend on it |
| ADR 002 D4 | No configuration, polling setting or feature flag |
| ADR 002 D5 | One language-neutral data method through the existing C1 contract |

**Requirements raised to mechanism owners:** none. The actual unmet need is
client access to WorkspaceTools' existing output resource; its implementation
and verification belong within that owner's scope using C1. The public surface is
the one method above; native capabilities and CLI own its implementation.

Ablation: keep one bounded snapshot instead of output events, a stream, offsets,
an arbitrary file API, another capture or persistent task state. Each is
unnecessary for displaying live output in the selected command detail.

## Pending task inventory

The existing WorkspaceTools Session part registers nonmutating `session.exec.pending`.
Parameters are `sessionId`, `branchId`, optional published `through` and optional property-query
`after` cursor. Reply: `branchId`, fixed `through`, `tasks`, `next`; at most 128 tasks per page,
ascending by start sequence. An ended branch is readable through the branch facade.

A task has `eventId`, `sequence`, `startedAt`, `taskId`, `turnId`, `requestId`, `callId`,
`description` and optional `title`, omitted when the owner fact has none
([WorkspaceTools' title contract](../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)).
These fields come from `exec/started` with no matching `exec/ended` at that cutoff.
The query does not read captured output, output paths, tool arguments or results.
Later `exec/ended` settles the inventory by `taskId`. Read failure leaves controls unavailable;
it does not prove that no tasks exist. This adds no task table and does not change stop admission.
