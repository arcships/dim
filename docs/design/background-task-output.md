# Background command output in the CLI

Status: Ready for implementation. The method is not implemented. Scope: the
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
[BackgroundExec](../architecture/rust-agent/plugins/BackgroundExec.md), not Rpc
or the client.

Existing alternatives do not supply a client read:

- Timeline observation supplies start/end facts, without live output chunks.
- `wait` waits for a notice; it does not read the running process.
- The `read` tool can read an output file, but no client method invokes that
  tool. Reading a daemon path from the CLI assumes both run on the same machine.
- Session files hold immutable stored attachments, not this active output file.

Expose the existing preview through the existing WorkspaceTools Session part.
No output event stream, file-read endpoint, new Capability or core mechanism is
needed. This contract does not settle member controls (#263), recently ended
task retention, full-file browsing, or Workflow task presentation.

## 2. Client method

WorkspaceTools contributes the leaf namespace **`session.exec.output`** through
C1 `sessionMethods`; it declares one method of the same name:

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
There is no caller-supplied path, byte range, cursor, size limit or subscription.

The leaf namespace matters: C1 rejects a namespace that contains an exact
built-in. `session.exec` would contain `session.exec.stop`; the sibling
`session.exec.output` does not. Record the leaf with the existing
`AgentBuilder::session_methods` and provide it from WorkspaceTools; keep Rpc's
built-in list and namespace checks unchanged.

Each call resolves the named branch through WorkspaceTools' existing
`TreeCommands`, then asks that branch's BackgroundExec for the active task's
capture. No Session part retains branch business state. A task id from another
branch or Session cannot select an output file.

| Condition | Result |
| --- | --- |
| Active task, including no output yet | Current preview in `text` |
| Task unknown, no longer active, or only present in replay | Existing `exec_task_unknown` application error, with message `No live output is available for this task`; never open a path from the request or historical events |
| WorkspaceTools is not installed, such as the shell-only assembly | Existing `unknown_method`; no substitute file read |
| Branch/provider unavailable or unloading | Existing branch/C1 availability and interruption errors |
| Malformed request | Existing invalid-params error |

Command completion remains authoritative through `exec/ended`. A final read can
race completion: it may return a last preview or `exec_task_unknown`. Neither
response creates a terminal fact. If a listed task returns `exec_task_unknown`,
the UI keeps its last preview, shows `Live output unavailable`, and stops
refreshing without marking the task failed or ended. After receiving the real
terminal event, it displays the existing final result. A read failure does not
stop the command or change its task status.

## 3. Ownership and lifecycle

The active capture is an existing I/O resource, but BackgroundExec's task table
does not retain a read handle today. Add an optional handle to that same capture
to the existing task record; present means live capture, absent means unavailable.
The table also retains ended/replayed tasks, so table membership does not imply
live output. Release the handle after the supervisor's terminal append attempt
settles, including on write failure; do not release it between process settlement
and that append. The read must not extend the existing resource cleanup lifetime.
Use the capture's existing synchronization, with no new lock or second output
buffer. Release the task-table guard before awaiting the capture read.

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
document itself supplies no runtime verification or complete #264 delivery.

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
