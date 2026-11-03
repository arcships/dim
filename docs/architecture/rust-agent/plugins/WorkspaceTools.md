# WorkspaceTools

The WorkspaceTools plugin registers WorkspaceTools in every Session and FileReads and BackgroundExec in every branch, and holds the file-write coordinator its Session part uses.

## WorkspaceTools

Owner: [workspace_tools/mod.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/mod.rs),
[files.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/files.rs),
[exec.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/exec.rs).
Provides ToolHost for read, write, edit, glob, grep, exec and exec_stop, and registers the
leaf namespace `session.exec.output` with `sessionMethods` for the active
command's bounded tail preview. It also registers the namespace `session.files`
with `sessionMethods`: `session.files.search` returns the files of the Session's
workspace an `@` mention offers, ranked as the TUI picker ranks them
(`workspace_tools/mentions.rs`); a query that names a path outside the workspace
finds nothing. The WorkspaceTools plugin registers this part
and the branch parts FileReads and BackgroundExec, and holds the file-write
coordinator: one per-path FIFO admission domain shared by write and edit across
every Session it serves, which it hands to each Session's part. The plugin keeps
the same coordinator across its generations and theirs, so old calls settle in
the domain new ones enter.

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Home, shell, environment and process supervision. |
| `S:TreeCommands` | Resolve the calling branch per call. |
| `S:SessionConfig` | cwd and collaboration role. |
| `S:SessionIdentity` | Session-specific output directory. |
| `S:SessionMethods` | Register the `session.exec.output` namespace, until this part unloads. |

Exec looks up the optional `S:FileAccessFacts` service when preparing a process,
to recognize its resolved cwd and obtain confirmed additional workspace roots
([worktree access R3](../../../design/worktree-access.md#r3-新进程的工作目录与隔离范围)).
Its absence retains the original workspace boundary and does not gate loading.

### Events and lifetime

No direct Dispatcher subscription. Tool results return to ToolExecutor;
background exec transfers ownership to BackgroundExec, which commits ends and
wakes. Per-call lookups obtain FileReads/BackgroundExec on the caller's branch.
Generation cancellation rejects new calls and stops accepted calls; cleanup
waits for tracked calls, including handoff. File mutations stage complete bytes
and retain coordinator admission through I/O/publication. Foreground commands
release their process trees before returning; collaboration members do not
background commands. Output files are not deleted simply by plugin unload.
A result that carries `outputPath` registers that path with `sessionFiles` before
it leaves the part; a registration failure fails the call (ADR 013 D2). A
`SessionFilesHost` in the process is read without injection, so a host without
native storage keeps working and its output stays unowned.
Before admission, `ToolHost.resolve_write` resolves a native `write` or `edit`
target and returns that same call with an absolute canonical `path`. ToolExecutor
uses it for approval and execution; the original model call remains in history.
Resolution only reads metadata, honors cancellation and rejects unresolvable or
non-UTF-8 paths. Permissions/approvals occur upstream of execution: these native tools are not sandboxed.
Unsupported metadata preservation fails unchanged; post-install errors require
re-reading before retry.

### Shared command construction and output

Exec uses [`HostPlatform::build_process_spec(command, cwd).await`](HostPlatform.md#command-construction).
HostPlatform owns the shell read, argv, environment and runtime PATH construction;
exec uses the returned shell for its own policies. Refusals carry named `code`
and `reason` fields. `cwd` is the resolved working directory, and missing-directory
errors retain the builder's resolved path without rewriting its message.

On macOS/Linux the exec declaration adds HostPlatform's static boundary line
after its environment line, so it stays stable for the Session. Exec keeps the
`Isolation` marker that preparation returns with its process. An unsuccessful exit, in
the foreground or after the background handoff, appends HostPlatform's failure
note to the result text when the shown output matches; stops, timeouts and
offload failures get none. Codes and structured content are unchanged.

`exec_stop {taskId}` stops a detached command owned by the calling branch through
BackgroundExec. It waits for cleanup and returns the committed terminal
`{task_id, status, result}` in the model-readable content. An already-ended task
keeps its result. Only `cancelled` confirms cancellation; `failed` remains an error,
including when the process tree could not be released. An unknown task fails
without stopping any command. Detached exec results name this operation and
distinguish their Task ID from Collaboration's Agent and Session identities.

```text
exec(command, runInBackground) -> Task ID: exec-...
exec_stop(taskId)              -> {status: cancelled, result: ...}
agent(cancel, agentId, sessionId) -> member task only
```

Exec retains its `cwd` argument resolution, POSIX `&` refusal, heavy-search
refusal, timeouts and background ownership. Its existing `exec_shell_unavailable`
tool-result code is preserved, with instructions to fix the file and retry.

Exec takes no input that selects a boundary. The user's `Process isolation` control in
`/permissions` or `/settings` selects the existing `sandbox` and `network` settings
([process isolation](../../../design/host-platform-isolation.md#process-isolation));
Permission presets and tool lists do not change them. The model cannot supply extra roots or widen the boundary. A call
that still sends `sandbox` is rejected as `exec_invalid_request`, and the background handoff
keeps the boundary the process started with
([HostPlatform](HostPlatform.md#events-and-lifetime)).

The same module exports the existing `Decoder::{write,end}`, `sanitize`,
`truncate_preview` and `EXIT_STDIO_GRACE`, and for ShellInput `capture_preview`,
`tail_bytes`, `signal_name` and `pump`. Keep one incremental decoder per pipe;
finish it at EOF or when the read grace ends. Invalid/truncated bytes become
`<0xNN>`. Sanitization removes control characters except tab/CR/LF, and
U+FFF9–U+FFFB. `capture_preview` keeps the bounded output tail (the last
100 KiB from a line start) that `truncate_preview` cuts to the last 2000 lines
within 50 KiB on UTF-8 boundaries. `pump` reads one pipe through its own
decoder into an `OutputSink` (exec's sink also offloads to the output file) and
keeps reading for the 100 ms grace after the root exits. These are the existing
output rules, exposed without changing their behavior.

### read results and images

`read` returns a PNG, JPEG, WebP or GIF file as an image block and one text
line, recognized from the file's content rather than its extension
([images.rs](../../../../crates/dim-agent-capabilities/src/images.rs)). The line
gives the path and the pixel size after any EXIF orientation; an image that had
to be re-encoded also gives the size sent, in pixels and bytes; an animation
says only its first frame was sent; `offset` or `limit` on an image adds one
sentence saying they do not apply.

Recognition comes before the extension refusal. The stat and directory checks
run first, then `read` opens the file and reads its head: content that is an
image takes the image path whatever the file is called, a PNG named
`screenshot.bin` included, and only content that is not an image is refused by
its extension, or by the text rules when it has none. A binary-extension file
is therefore opened before the refusal: a missing one reports `file_not_found`
and an unreadable one `read_error`.

One shared function, `images::scale_image`, makes the image acceptable to a
model. It reads from a seekable source — the file, not its bytes — so the whole
source is never buffered: it reads the header, runs the guard, and only then
decodes. An image whose longest edge exceeds 2048 px or whose base64 encoding
exceeds 5,000,000 bytes is decoded, its EXIF orientation applied first so the
sent pixels are upright, and re-encoded to fit: a PNG source stays PNG while
its scaled PNG fits, everything else becomes JPEG at quality 85, transparent
pixels flattened onto white, and the long edge steps down until the encoding
fits. An image within both limits with no orientation to apply is sent byte
for byte. An animation is always re-encoded from its first frame, its EXIF
orientation applied the same way, so the frame sent is upright as a still's
pixels are, its transparent pixels flattened onto white the same way. The line's pixel size is the source's size after its EXIF
orientation, so it compares with the size sent. One guard keeps memory bounded:
a header claiming more than 100 megapixels is refused from its dimensions
before the pixels are decoded, and the refusal names the dimensions and asks
for a smaller copy.
Decoding is bounded by that guard too: a decoder that takes an allocation
limit may allocate at most 800 MB, the guard at eight bytes per pixel — the
largest format `read` decodes, 16-bit RGBA — and an allocation refused for size
takes the pixel-guard form; the WebP decoder takes no allocation limit of its
own, so a metadata chunk larger than that bound, or than the file that declares
it, is refused before it is read.
HEIC, HEIF and AVIF are refused with the format named; every other binary file
is refused as before. `read` never decides for the model: the same image block
goes into the tool result whatever the model, and the stored result holds what
was sent, the scaled image. ModelProvider turns the image into its text
placeholder for a model without image input
([ModelProvider](ModelProvider.md#image-handling)).

The model-facing texts and their classes
([model prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):

- **Protocol.** The image text line's form — `Read image: <path> (<w>x<h> px).`,
  the `Sent <w>x<h> px as <media type> (<n> bytes).` sentence when the image was
  re-encoded, `Animated <format>: only the first frame was sent.` for an
  animation, and `Offset and limit do not apply to images.` when either was
  given — the refusal sentences (unsupported format, too many pixels,
  undecodable bytes) with their `binary_unsupported` code, the tool
  declaration's schema and image result format, and the description's sentence
  naming the formats `read` returns.
- The declarations' marks are stated once, in [Model-facing
  texts](#model-facing-texts).

### Declared effects

`write`, `edit` and `exec` declare the effect of each call on its result, through
the tool contract's `effects` field
([session boundary A.6](../../../design/contracts/session-boundary.md#a6-tool-effects)).
A declaration states what the call did, not what it was asked to do, and rides on
the result the executor records on `step/tool-result`, the only fact a declaration
is written to.

- `write` and `edit` declare `fileWritten` with the resolved path once the
  replacement is published, including the parent-sync failure whose result warns
  that the file may have been partially written: the file changed either way.
  They declare nothing when the call fails before the publish or is cancelled
  before it.
- `exec` declares `commandRun` for every command whose process started:
  - the exit status the result observed, `exitCode` for a code and `signal` for a
    name, a non-zero exit included although its result is an error;
  - no exit status when the command is handed to the background, at once or
    after the foreground wait: the call observed that the command started and
    nothing more, and its later outcome reaches the model as BackgroundExec's
    wake notice, not as a declaration;
  - no exit status when the result observed none: the command was stopped by the
    tool, its output was lost, its handoff to the background failed, or the call
    was cancelled after the command started. A cancelled call still declares:
    the tool stops the command and returns its own result, with the signal only
    if one was observed.

  A command that never started — refused input, a refused high-cost scan, a
  boundary that could not be prepared, an unavailable shell, a cancel before the spawn —
  declares nothing.
- A `write` or `edit` cancelled before it publishes is settled by ToolExecutor
  without a result of its own, so it carries no declaration.
- The tools declare the files they change in the workspace. Their own temporary
  and output files — the staging file `write` and `edit` install, the file a
  large `read` result spills to, and the output log an `exec` command writes —
  are the call's delivery, not its effect, and are not declared.
- `read`, `glob` and `grep` declare nothing else: they neither change a
  workspace file nor run a command.

### Rule check

The read-only [`session.exec.output`](../../../design/background-task-output.md)
method resolves the requested branch through TreeCommands and reads that branch's
BackgroundExec capture. It accepts only `{sessionId, branchId, taskId}` and returns
`{text}`. C1 owns admission and unload settlement on WorkspaceTools' original
context. The method does not open a client-supplied or historical path, append
events, acquire Session holds or start background work.

- §1 and ADR 002 D1–D3: command construction belongs to HostPlatform; no new
  plugin, wiring layer, state owner or dependency is added.
- §4 rules 1–6 and 9: existing ToolExecutor results and BackgroundExec handover
  are unchanged; no new events, before callbacks or context path are introduced.
- §4 rule 7: callers reuse the public builder and HostPlatform service; no
  consumer reads another plugin's private state.
- ADR 002 D4: exec reads shell configuration for each invocation, not on reload.
- ADR 002 D5: the builder is an existing native execution API; the output read is
  a language-neutral C1 method. Refusals are explicit code/reason data; no new
  runtime binding mechanism is introduced.

### Requirements raised to mechanism owners

None (§4 rule 8). S7 uses the existing host and storage contracts.

### Model-facing texts

The `read`, `write`, `edit`, `glob`, `grep`, `exec` and `exec_stop` declarations
are the owner's, and their parts are marked here
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):
every schema is protocol; a description's sentence that states what the call
does, what it returns or its reach is protocol; a sentence that advises when or
how to use it is policy. A name that a policy sentence states stays protocol.

- `read` — protocol: "Read a file from the filesystem.", "Text comes back with
  line numbers" and "A PNG, JPEG, WebP or GIF image comes back as an image block
  with one line giving its path and pixel size."; policy: "use offset and limit
  for large files".
- `write` — protocol throughout: it states what the call writes, that it creates
  the file and that it overwrites the whole content.
- `edit` — protocol: "Edit an existing file by replacing exact text matches."
  and "Supports single replacement (default) or replacing all matches with
  replaceAll."; policy: "Read the file first, then derive oldText and newText
  from the latest read result." and "Use this tool for ordinary incremental text
  edits instead of shell commands or Python."
- `glob` — protocol: "Find files with a safe case-insensitive git-style glob."
  and "Returns a bounded preview"; policy: "narrow path or pattern when
  truncated".
- `grep` — protocol throughout: it states what it searches and that it returns a
  bounded preview with at most 5 matches per file.
- `exec` — protocol: the environment and isolation lines and "Execute a command
  using the shell shown in the environment context. stdout and stderr are
  merged. Long-running commands automatically move to background."; policy: the
  six bullets on which tool to use for text operations, which searches to keep
  narrow, when to set `timeoutMs` or `runInBackground`, and waiting with `wait`
  instead of `sleep`.
- `exec_stop` — protocol throughout: it names the `Task ID` handle `exec`
  returns, the terminal `{task_id, status, result}` format, the `cancelled` and
  `failed` statuses, and that Collaboration's `agentId` and `sessionId` are not
  exec task IDs.

A policy change is judged against the current text by
[§8](../../../design/model-prompts.md#8-judging-a-change); a protocol change
lands with the behaviour and the flow test that assert it. Four descriptions
carry policy sentences — `read`, `edit`, `glob` and `exec` — and none of them
has a factory member yet; each gets one in the change that first evaluates a
candidate for it.

Of their results, the texts that carry a handle or a status are protocol:
the `exec` result that moved to the background writes `Task ID: <id>`,
`Output: <path>` and "Stop with exec_stop({"taskId":"<Task ID>"}); this is not a
Collaboration agentId.", and a task whose owner restarted reports
"Background task <id> did not survive a restart of its owner; its final result is
unknown. Output so far: <path>." The model carries the task id to `wait` and
`exec_stop` and the path to `read`, so a change to either format changes what the
model can do.

`exec_stop`'s result is the task's terminal `{task_id, status, result}` JSON, and
it is protocol too: the model reads `status`, and only `cancelled` confirms the
stop, while `failed` stays an error. `exec_stop`'s refusals are protocol, as
Wait's are, because each names the input or the handle it refused: "exec_stop
requires the taskId returned by exec: <error>", "exec_stop taskId must not be
empty." and "Command is not a task of this branch. Use the Task ID returned by
exec on this branch; use agent for Collaboration members. No command was
stopped."

The other tools' refusal codes (`invalid_input`, `not_regular_file`,
`is_directory`, `file_modified_since_read`, `text_binding_required`,
`permission_denied`, `write_error`, `exec_invalid_request`,
`exec_task_unknown`, `exec_terminate_failed`, `exec_wait_failed`,
`exec_output_offload_failed`) reach clients and persistence in
`structuredContent`; `ToolResult.content` carries the sentence the model reads,
and HistoryRenderer renders the content alone, so no code reaches the model. A
tool that starts writing one into `content` moves that text into this section.

### Tests

[workspace_files.rs](../../../../crates/dim-agent-capabilities/tests/workspace_files.rs),
[local_execution.rs](../../../../tests/integration/tests/local_execution.rs),
[local_product.rs](../../../../tests/integration/tests/local_product.rs).

[workspace_files.rs](../../../../crates/dim-agent-capabilities/tests/workspace_files.rs)
also drives `read` over images: a PNG and its line, an image recognized ahead of
the binary-extension refusal, a JPEG scaled within both limits, an image
re-encoded when its base64 exceeds the limit, the 3,750,000-byte exact fit sent
byte for byte and one byte over it re-encoded, an EXIF-rotated photo and an
animated WebP's first frame made upright, a wide screenshot kept PNG, an
animated GIF's first frame, transparent pixels on white, a WebP EXIF chunk
larger than its file refused, a missing file reported as `file_not_found`, the
pixel guard before decoding, and the refusals.
[read_image_flow.rs](../../../../crates/dim-app-server/tests/read_image_flow.rs)
runs a model's `read` on a PNG: a vision model receives the image block, and a
model without image input receives the placeholder.

The App Server's [native execution flow](../../../../crates/dim-app-server/tests/transport_flow/shell_input.rs)
reads a live capture through C1, holds its real SQLite terminal commit, checks
bounds and branch/Session ownership, then verifies replay and close behavior.
The existing idle-unload/recovery flow in `local_execution.rs` also checks an
empty live preview and unavailable historical captures.

<!-- generated:plugin WorkspaceTools — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:HostPlatform`、`S:SessionConfig`、`S:SessionIdentity`、`S:SessionMethods`、`S:TreeCommands` |
| 提供 | `S:ToolHost` |
| 被注入于 | `ToolRegistry` |
<!-- /generated -->

## FileReads

Owner: [workspace_tools/file_reads.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/file_reads.rs).
Records canonical-path bindings for files read on this branch: raw-byte revision,
encoding, BOM and line endings. Existing-file `edit` admission requires a binding
established by that branch. Existing-file `write` admission requires one whose
operation — a `read`, `write` or `edit` on this branch — is still in the model's
context: a completed `context/compaction` on this branch that cut the operation
out refuses the write with `text_binding_required`, and the model reads the file
again (a part is enough) or uses `edit`.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Timeline` | The position of the operation that bound a file, and the branch's completed `context/compaction` facts, read at `write` admission. |

WorkspaceTools resolves this branch service per file-tool call, rather than
injecting it into the Session plugin.

### Events and lifetime

No events, tasks or external resources. A mutex-protected map is created in each
`apply`. Each binding records the sequence its operation started at, and `write`
reads the branch's later facts at admission, so a compaction committed before the
admission is always seen. Reload loses its bindings; it does not reconstruct them
from history. A fresh read is therefore needed before mutation. The record is not
a filesystem lock, permission grant or cross-process compare-and-swap.

### Tests

[workspace_files.rs](../../../../crates/dim-agent-capabilities/tests/workspace_files.rs):
`writes_require_a_fresh_read_on_the_same_branch`,
`a_failed_offload_does_not_bind_the_file`,
`edits_and_writes_preserve_bom_and_line_endings`.
[context_flow/compacted_bindings.rs](../../../../crates/dim-agent-capabilities/tests/context_flow/compacted_bindings.rs):
`a_write_after_a_compaction_that_cut_its_read_is_refused_until_the_file_is_read_again`,
`a_compaction_that_keeps_the_read_leaves_the_write_admitted`,
`an_edit_after_a_compaction_that_cut_its_read_succeeds`,
`a_compaction_that_cuts_a_written_binding_refuses_the_next_write`.

<!-- generated:plugin FileReads — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Timeline` |
| 提供 | `B:FileReads` |
| 被注入于 | — |
<!-- /generated -->

## BackgroundExec

Owner: [workspace_tools/background.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs).
Owns commands transferred from foreground execution, their terminal records and
bounded completion notices on the calling branch.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Dispatcher` | Replay commands, then consume command ends and interrupts in dispatch order. |
| `B:Timeline` | Commit ends/notices, recovered ones included. |
| `B:TimelineIdentity` | The branch whose entry this part registers. |
| `S:GuideEntries` | Register the exec notification's entry for this branch, until this part unloads. |
| `S:SessionLifecycle` | `hold()`: each running task holds the Session, so an idle unload waits for it. |

### Events and lifetime

Produces `exec/ended` and tagged `input/message` wake notices; consumes
`exec/ended` and `stop/interrupt`. A turn stop leaves background commands running;
a branch close cancels them without a wake. Explicit task stop, a reload of this
capability and a Session close cancel and release process trees before
settlement; an idle unload is refused while a task runs. Each task holds its
Session from `start`, before `exec/started`, until its supervisor has appended
`exec/ended`; that end keeps the branch busy until its wake is posted. A start
that gets no hold (the Session is closing) is refused as in a closing branch,
and a refused start stops its command. Tracked tasks are joined on unload.
Replay reposts unreported non-cancelled ends but never restarts a historical
process. The notice is not the full command output; output files carry large
results. Its escaped result preview uses the existing context-block 16 KiB limit,
with the command/status and full-result read hint around it. An end dispatched
after branch-close interruption must not wake it.

Each live task retains its existing bounded output capture until the supervisor's
terminal append attempt settles, including on failure. `output(task_id)` reads
that preview after releasing the task-table guard. Ended and replayed entries
have no capture; a historical path never reconstructs one. WorkspaceTools exposes
this finite read through its existing Session methods contribution.

### Model-facing texts

One text reaches the model, and it is mixed
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):
the notice's element `dim-notification`, its attributes `source="exec"` and `task`,
the `Background command "<name>" <status>.` line, the `Output preview:` line and
the full-result hint are protocol; "It is information, not a new request." and
"If the work it belongs to is no longer wanted, note the result and do not restart
that work." are policy. The entry's text stays code
([Prompt §5](../../../design/plugins/Prompt.md#5-facts-that-reproduce-it)).

The notice carries its guide entry: this part registers it from `apply`,
branch-scoped, and a request on that branch after the notice carries the entry
([Prompt §4](../../../design/plugins/Prompt.md#4-the-built-in-entries-and-their-writers)).

### Tests

[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`background_completion_is_recorded_once_and_wakes_with_a_bounded_notice`,
`a_turn_stop_keeps_background_work_and_its_wake_starts_a_turn`,
`idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end`,
`running_tasks_hold_the_session_until_the_last_one_ends`,
`a_reload_or_a_closing_session_refuses_a_start_and_leaves_no_hold`.

<!-- generated:plugin BackgroundExec — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:GuideEntries`、`S:SessionLifecycle` |
| 提供 | `B:BackgroundExec` |
| 被注入于 | — |
<!-- /generated -->
