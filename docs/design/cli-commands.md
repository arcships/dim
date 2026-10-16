# CLI commands and exports

F6 (#77) covers the command entry points, conversation export and JSONL
execution listed in [overview Appendix D](overview.md#appendix-d-cli-layer) and
the [feature list](../feature-list.md#cli-commands). The native CLI remains a
client of the existing daemon. This document defines the CLI interface for
the implementation; the [native CLI contract](../../crates/dim-cli/README.md)
describes the current executable.

The proposed [attachment input contract](cli-attachment-input.md) defines the idle draft editor,
local image sources, paste handling and exec image arguments.

## Commands

| Entry | Behavior |
| --- | --- |
| `dimcode tui` | Open the existing interactive scrollback client. |
| `dimcode exec [-p TEXT] [--json]` | Run one prompt and exit; read redirected stdin when `-p` is absent. |
| `dimcode exec resume SESSION_ID [-p TEXT] [--json]` | Continue the named persisted Session with one prompt. |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | Export the main branch; write to stdout when no output path is supplied. |
| `/export markdown PATH` or `/export json PATH` | Export the current main branch to a file while the interactive client is idle. |
| `/exit` | Detach and exit from the idle interactive prompt. |
| `!command` / `!!command` | Call the Session's existing `shell.run` in record/private mode, including during a model turn. |

Global home, workspace, server and model-profile options retain their current
meanings and work before or after the command. Existing login/account/logout,
models, MCP and daemon commands remain available. Existing `--prompt`, piped
stdin and `--resume` calls retain their behavior; `exec resume` also accepts the
same existing global options. Supplying the Session twice or the prompt twice
is an argument error. `--json` is an `exec` option, and `tui` requires a terminal.
Help/version and argument errors do not start the daemon.

The interactive export parser reads the format token and treats the remainder
of the line as a literal path, including spaces, without shell evaluation or
quote expansion. Missing paths and unsupported formats print usage and return
to the prompt without submitting input. `/exit` takes no arguments. Only these
exact command names are intercepted; other text follows the normal input path.
Subcommands do not create a plugin command registry.

P1 shell prefixes apply only to interactive input. Their execution, recording,
progress, cancellation and lifetime follow [ShellInput](plugins/ShellInput.md).
The CLI generates `runId` before sending the request and never retries a run.
The command after the prefix is passed verbatim; an empty command prints usage.
While a command is pending, a second shell line reports that the caller already
has a run. Another client's run can still cause the server's `shell_busy` error.
Neither case submits model input. Ordinary text typed during execution retains
the native client's discard behavior, with a message to wait for the next prompt;
there is no input queue. A pending approval owns its answer line, including `!`.

The first Ctrl+C during a shell run sends `shell.stop` for its client-generated
identity, including before admission. A second interrupt uses the existing model
turn stop and waits for settlement; it never skips pending replies. EOF stops
the caller's shell run and model turn before detaching. With no run, the existing
interrupt behavior applies. Progress is labelled as incomplete; the final reply
prints the authoritative bounded output again, its status, truncation, and the
recorded sequence or recording error. Output is filtered at the terminal boundary.
RPC errors remain command errors, never model text. A disconnected caller does
not imply that a run stopped or that its recorded input was lost.

```text
> Explain the failing build
Assistant: Inspecting the build...
!cargo check
[shell started] record
[shell progress] (incomplete)
    Checking project
^C
[shell stopping] Waiting for command settlement
^C
[stopping] Waiting for resource settlement
[shell output] (final)
    Checking project
[shell stopped] ...
[shell recorded] sequence: 42

> !!printf private
[shell started] private
[shell output] (final)
private
[shell completed] exit: 0

> _
```

These records use terminal scrollback and wrap at the terminal's width; there
is no second prompt, overlay or client-side process. Best-effort progress may
be dropped. Final output is shown in full rather than inferred from progress.

For both export entries, relative output paths use the CLI process's current
directory, including after resume; neither the Session's cwd nor `--home` changes
that base. The CLI treats `~` literally. Usage/help states: `PATH is relative to
the CLI current directory; ~ is literal; use an absolute path for another base`.
An invoking shell may expand an argument before the CLI receives it.

Other target commands require their owning features. F6 does not implement a
fullscreen interface, persistent RPC/ACP server, verification/repair loop,
provider editor, Session deletion/fork, or model-request snapshot export.
`dimcode export` remains reserved for the distinct request snapshot feature.

## Terminal interaction

```text
$ dimcode tui --home /tmp/dim-work
dimcode · model: provider/model
workspace: /workspace/project
session: <session-id>

> /export markdown
Usage: /export <markdown|json> PATH
PATH is relative to the CLI current directory; ~ is literal.

> /export markdown /tmp/Session notes.md
Exported Markdown: /tmp/Session notes.md

> /export json /tmp/events.json
Exported JSON: /tmp/events.json

> /exit
$

$ dimcode session export <session-id> --format markdown > notes.md
$ dimcode exec -p 'Explain this project' --json > run.jsonl
```

The prompt keeps keyboard focus and the terminal keeps scrollback. There are no
new dialogs, overlays or fixed-width columns. Paths and error messages use the
existing terminal text filter; narrow terminals wrap normally. An empty Session
exports an empty event list or a Markdown heading without fabricated messages.

## Execution and JSONL output

`exec` uses the existing create/attach/history/submit/idle/detach sequence and
stop settlement. A resumed running turn settles before the new prompt is
submitted. As the attached interaction client, `exec` actively sends the ordinary
denial response to an approval request, stops the turn, waits for stop
settlement, and exits with code 3. It does not change
`SessionConfig.interactive`. Sessions created as noninteractive retain Approval's existing owner-side policy (#98 D10).
Other exit codes retain the native CLI contract, including 2 for turn failure
and 130 for interruption. A transport failure does not retry a submission.

In JSON mode, stdout contains one compact JSON-RPC notification per line using
the daemon's existing `method` and `params`. Only notifications for the selected
Session are emitted. Protocol handshake, RPC replies and other Sessions never
enter this stream. Session handles and human diagnostics go to stderr; normal
assistant/tool rendering does not also print to stdout.

```json
{"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"s","branchId":"main","requestId":"r","textOffset":0,"text":"Hello"}}
```

Notification fields remain defined by the existing public v3 protocol.
Live observations begin after attach; stored
history is used for reconciliation and is not dumped as execution output.
Durable events retain their sequence and public payload, including fields the
text display does not render. Model progress retains its existing method and
offsets: a progress notification is not an additional durable text fact.
JSON encoding escapes control characters; the terminal text filter does not
rewrite data in machine output. The CLI adds no synthetic lifecycle events or
second event schema. Notification delivery keeps the daemon's existing
best-effort semantics; consumers needing a fixed complete record use export.

## Conversation export

Both entries call the existing `session.history` RPC. The first response fixes
`through`; subsequent pages use that watermark and the server's `nextAfter`,
including gaps caused by filtered records. The CLI reuses its existing cursor
checks. Export does not submit input, change model context, close the Session,
or implement a server-side exporter.

The exported branch is always `main`; child branch histories are not fetched or
included. Markdown renders each `branch/create` fact with its child id as
`Child branch <id> (contents not exported).` JSON preserves that main-branch
event unchanged. `branch/create` records a request before the child is built.
For `branch/create-failed`, Markdown uses the event's `branch` field to render
`Child branch <id> was not created.` for both failed and cancelled creation.
JSON retains the failure fact unchanged. No child history is fetched to determine
the outcome. A reader can distinguish failed creation from omitted child content.

JSON output contains `{sessionId, branchId, through, events}`. Each event is the
public history event without dropping unknown fields or event kinds. This is
a conversation export, not a restore/import file or native database format.
Markdown renders user/assistant content in history order and clearly labels
reasoning, tool calls/results and non-text attachments. It does not execute
content, resolve attachment URLs, or replay tools. It preserves code fences
without allowing a message to close the surrounding tool-result fence.

The CLI reads the fixed prefix successfully before opening an output file.
Files use create-new semantics: existing paths fail without replacement.
On Unix they are created with mode 0600 through the standard OpenOptions
extension; Windows uses the native inherited ACL. Existing files are never
opened to change their permissions.
No output file is created after a history error. A write failure is reported
and may leave a partial newly created file; no success message is printed.
Export-to-stdout failures return a nonzero code. Interactive export errors
return to the input prompt and never become model input.

## Implementation and verification

Changes stay in `crates/dim-cli`: `args.rs`/`main.rs` select commands,
`session.rs` reuses Session execution and its shared paged-history reader,
`display.rs` keeps human rendering separate from JSON output, and `export.rs`
formats conversations and writes new files. Conversation formatting belongs to
the CLI. No App Server, Capability, core lifecycle, protocol, storage migration,
dependency or runtime registration changes are required.

Reuse `cli_flow.rs` and its actual daemon profile fixture to cover `exec` and
resume, stdout/stderr separation, public JSON notification preservation,
turn/approval failure and stop settlement, fixed-watermark multi-page export,
both formats, existing-file rejection, and interactive command interception.
Cover relative paths when the CLI and Session directories differ, literal `~`
usage, child-branch omission/markers, failed or cancelled child creation, and
Unix file privacy.
Use isolated homes only. Implementation requires the focused CLI flows and
affected Clippy.
