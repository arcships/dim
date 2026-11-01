# CLI commands and exports

F6 (#77) covers the command entry points, conversation export and JSONL
execution listed in [overview Appendix D](overview.md#appendix-d-cli-layer) and
the [feature list](../feature-list.md#cli-commands). The native CLI remains a
client of the existing daemon. This document defines the CLI interface for
the implementation; the [native CLI contract](../../crates/dim-cli/README.md)
describes the current executable.

The [attachment input contract](cli-attachment-input.md) defines the idle draft editor,
local image sources and paste handling. The multiline/paste editor and headless exec image
arguments, clipboard/file insertion, completion and dropped paths are implemented. The attachment
contract records the native platform acceptance evidence and outstanding checks.

## Command menu

In both scrollback and fullscreen TUI, group commands by category and sort
categories and command names alphabetically by their canonical English identifiers.
Search relevance takes priority; equally ranked matches use the same category/name order.
Exit and suspend require their exact name or alias before dispatch: choosing an
incomplete match fills the command name and keeps the composer open. Exact typed
commands, including hidden commands, retain their own dispatch behavior.
The command menu lists `/permissions` as the permission entry. `/approvals` and `/approval`
remain compatibility aliases when typed directly. Interrupt and steer are reached through
`Esc` and `Ctrl+S` and are omitted from the menu. The menu only lists slash
commands: the view actions for picking an earlier message and scrolling to the
top or bottom are also omitted; `Tab`, `Home` and `End` retain their bindings.

```text
Commands
  app     /exit        exit
  app     /keys        show the key bindings
  model   /permissions   file access, tool approval and process isolation

/ + Enter          -> /exit (completion only)
/exit + Enter      -> exit
/permissions + Enter -> permission controls

Esc     interrupt the run
Ctrl+S  steer with the next queued message
Tab     pick an earlier message to edit or quote
Home    scroll to the top (fullscreen, empty draft)
End     scroll to the bottom (fullscreen, empty draft)
```

### Queue strip

With a message queued, the strip above the composer draws the queue in a titled border: messages oldest first, the next to run at the top, then a hint naming the keys that act on them. With an empty draft, `↑` withdraws the newest queued message into the composer to edit; `Ctrl+S` steers with the next one. The hint names a command only when the command table has one for the queue, so a typed `/name` it shows runs locally instead of being sent to the model. No command views the queue, so the hint names keys only.

```text
╭─ Queue (2) ─────────────────────────────────────╮
│ › fix the failing test in session_host          │
│ › then update the design note                   │
│ ↑ edit · ctrl+s steer now                       │
╰─────────────────────────────────────────────────╯
```

A queued message the strip does not show adds its count to the hint: `+3 more · ↑ edit · ctrl+s steer now`. The strip shows at most five messages and keeps the height it reached while the queue drains.

## Usage report

In both TUI modes, `/usage` prints the main branch's context usage. When the
active model uses Dim OAuth, it also queries and prints the account's plan,
credits, rolling windows and feature meters through the existing
[account usage contract](../../crates/dim-oauth/docs/runtime-contract.md).
Other model providers keep context-only output; signing in alone does not
enable the account report. The `/login` account view remains available.

Context and account failures are reported independently: unavailable context
usage does not hide account usage, and an account error does not discard the
context report. Missing optional account fields stay absent. The illustration
below uses synthetic data; an account need not have a 5h window.

```text
/usage
  Context: 1,849 / 256,000 tokens (...) ...

  DimAgent OAuth
  Pro · active · expires 2026-11-30

  Credits     25,000 / 300,000  8%
  [###-------------------------------------]

  5h window   9,500 / 10,000  95%
  [######################################--]

  Web search  211 / 1,500  14%
  [######----------------------------------]
```

## Provider accounts

The native client signs in to every Provider whose `provider.management` entry
carries an [`account` descriptor](appserver/provider-management.md#31-read-models).
That descriptor is the only source: the client keeps no per-provider list, and
it compares no Provider id. The described accounts are the provider list; the
descriptor's `loginMethods` say which flow each one starts (`browser` or
`device`); and the calls are the descriptor's namespace's —
`<namespace>.login.begin` (device), `<namespace>.login.browser.begin`,
`<namespace>.login.browser.complete`, `<namespace>.login.status`,
`<namespace>.login.cancel` and `<namespace>.logout`. A namespace that the
daemon does not advertise is reported as unsupported instead of starting a
flow that cannot finish.

The `account` namespace is the Dim account. Its calls also carry `providerId`
and the account directory, and `/login` keeps its account panel — identity,
plan and usage, and the enterprise-email entry — because the descriptor carries
neither identity nor usage. Every other namespace is a single account whose
calls carry only the attempt id.

The terminal UI offers these operations on the `/connect` row of each described
account. A signed-out account says `Sign in`, and opening it starts its only
flow; a signed-in one shows its state and offers `Sign out`. A row whose
namespace is `account` opens the Dim account's `/login` panel instead, with its
identity, plan and usage. A Provider without a descriptor keeps the connection
form, and opening it starts nothing.

```text
/connect
╭─ Providers ──────────────────────────────────╮
│  OpenAI Codex                     Sign in    │
│  Grok Build                       Sign in    │
│› Add custom provider                         │
╰─ Enter open · Esc clear/close ───────────────╯
        │ Enter on a signed-out described row
        ▼
╭─ OpenAI Codex OAuth ─────────────────────────╮
│   Browser login                              │
│ https://…/authorize?…                        │
│   Callback URL: (paste here)                 │
│   Submit callback                            │
╰─ Select to copy · Enter opens link ──────────╯

╭─ Grok Build OAuth ───────────────────────────╮
│   Device login                               │
│   Enter code:    ABCD-EFGH                   │
│ https://…/device                             │
│   Restart device login                       │
│   Waiting for authorization                  │
╰─ Select to copy · Enter opens link ──────────╯
```

`dimcode login [PROVIDER]` starts the same flow in the line client and prints
the URL or the code the daemon returns; without a PROVIDER it lists the
described accounts. `dimcode logout PROVIDER` signs one out, and
`dimcode account PROVIDER` prints its non-secret state. `--help` lists these
forms and never contacts a daemon. A build without the Dim account provider
says so, and names the API-key Provider alternative, from each of these
commands.

## Process isolation settings

`/settings` and `/permissions` expose the same `Process isolation` control. It replaces the old
`Execution` panel and separate Sandbox entry; there is no duplicate toggle, native `dimcode
isolation` command, `/isolation` command or policy file. Choices and their stored boolean mapping
belong to the [process isolation design](host-platform-isolation.md#process-isolation), not to
Permission. File access and Tool approval never change this control.

```text
Settings                         Process isolation
  File access          >       > Off
  Tool approval        >         Sandboxed · No network
  Process isolation    >         Sandboxed · Network allowed
                                 Applies to new processes and persistent shells.
```

Opening the control reads `{source: "execution", scope: "user"}` through
[`settings.read`](appserver/settings-service.md) and retains its CAS revision. If advertised,
`host.execution.backend {}` supplies `{available: true}` or `{available: false, reason}`; show the
daemon's reason without inferring availability from the client platform. The choices remain
editable. ↑↓/Tab moves, Enter selects, Esc returns. Saving uses `settings.update` with the read
revision; a conflict is shown without retrying the user's choice against unseen data. Reload
discards the draft and reads again. Loading disables mutation; unsupported methods report an
unsupported-daemon error, never a local fallback. Reset requires confirmation and writes both
booleans false. Off may retain the old inert Network value; either sandboxed choice writes its
own network value explicitly.

This is user-scope host configuration, not branch policy or the boundary of a running command.
A running process, including a persistent shell, keeps its startup boundary. An unavailable
backend still refuses the next sandboxed start as `isolation_unavailable`; displaying the reason
does not weaken enforcement. The existing backend and execution source are reused.

A model permitted to write the settings source can change the next process's boundary, just as
a user can through this control; that write creates no grant and changes no running process
([ADR 011 D2](../adr/011-execution-settings.md)). File access is not OS isolation for shell code.

## Commands

| Entry | Behavior |
| --- | --- |
| `dimcode tui` | Open the existing interactive scrollback client. |
| `dimcode acp [--agent ID]` | Serve ACP over stdin/stdout through the client-host [AcpGateway](plugins/AcpGateway.md); lifecycle and unsupported features follow its contract. |
| `dimcode exec [-p TEXT] [--image PATH ...] [--json]` | Run text and/or local images and exit; read redirected stdin when `-p` is absent. |
| `dimcode exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]` | Continue the named persisted Session with text and/or images. |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | Export the main branch; write to stdout when no output path is supplied. |
| `/export markdown PATH` or `/export json PATH` | Export the current main branch to a file while the interactive client is idle. |
| `dimcode export SESSION_ID [--output PATH] [--database PATH]` | Write the Session's whole Timeline as versioned JSONL without a daemon ([Timeline export](appserver/timeline-export.md)); write to stdout when no output path is supplied. |
| `dimcode migration [run\|inspect] [--source OLD_HOME]` | Import by default, or inspect, a stopped legacy data home through the shared [Migration plugin](../../crates/dim-migration/README.md). |
| `dimcode migration list\|show TASK\|cancel TASK` | Query durable results or request cancellation without a daemon. |
| `dimcode migration retry TASK [--source OLD_HOME]` | Reconcile a previous task's source again; return a new task linked to the old report. |
| `dimcode hooks list [--cwd DIR]` | List the effective hook catalog over `hooks.catalog` ([Hooks](plugins/Hooks.md#a7-management-methods)): each source with its path, enablement and state, and each handler with its event, matcher, command and state. |
| `dimcode hooks enable\|disable [--cwd DIR]` | Enable or disable the project hooks of the workspace at `DIR` (the current directory by default) over `hooks.set_enabled`, recorded against the project root Settings authorizes for it. |
| `dimcode login [PROVIDER] [--directory URL] [--device] [--redirect-uri URI]` | Start the described account's browser or device sign-in ([Provider accounts](#provider-accounts)); without PROVIDER, list the described accounts. |
| `dimcode account PROVIDER` | Print the described account's non-secret state. |
| `dimcode logout PROVIDER` | Sign the described account out. |
| `/connect` | List the Providers `provider.management` reports; a described account opens its sign-in or sign-out step, and any other Provider opens the connection form. |
| `/exit` | Detach and exit from the idle interactive prompt. |
| `!command` / `!!command` | Call the Session's existing `shell.run` in record/private mode, including during a model turn. |
| `--permission read-only\|workspace-write\|full-access` | Select the preset of a new or resumed Session for `dimcode`, `tui`, `exec` and `exec resume` ([permission controls](#permission-controls)). |
| `/permissions` | Open independent File access, Tool approval and Process isolation controls; manage command rules while idle ([permission controls](#permission-controls)). |
| `/approvals`, `/approval` | Compatibility aliases for `/permissions`; legacy preset arguments remain supported. |

`dimcode hooks` never writes a hook document. The user's document is the Settings source `hooks`
of the home the daemon serves (`user/hooks` in native storage, `hooks.json` in a standalone home),
written by `hooks.save`; the project document is `.dimcode/hooks.json` in the repository, edited
there. `dimcode hooks enable|disable` records the workspace in the user-local `hooks-projects`, so a
project file cannot enable itself ([Hooks](plugins/Hooks.md#a7-management-methods)).

Global home, workspace, server and model-profile options retain their current
meanings and work before or after the command. `--server PATH` names a
standalone `dim-app-server` binary, which the CLI runs by its own name; the
default instead runs this executable's `app-server` subcommand, so naming this
executable is an error. A path that does not exist, a program that exits
before serving, or one that does not start serving in time is reported with the
same requirement, unless it left a [startup note](#daemon-startup-failure).
Existing login/account/logout, models, MCP and daemon commands remain
available. Existing `--prompt`, piped stdin and `--resume` calls retain their
behavior; `exec resume` also accepts the same existing global options.
Supplying the Session twice or the prompt twice is an argument error. `--json`
serves `exec` and `migration`, and `tui` requires a terminal. Help/version and
argument errors do not start the daemon.

### Daemon startup failure

All entries that start a daemon share the client startup in `connect_inner`.
The daemon is detached and its stderr is the append-only `daemon.log`, so a
per-launch note is the only channel that does not parse history or depend on a
pipe the client may close. Before spawning, the CLI removes
`<native home>/dimcode.sqlite.startup`, where the native home is the daemon's
own home, `<home>/rust`. When the spawned daemon exits before the client
connects, the CLI reads that note (at most 16 KiB, and only a regular
file; on Unix it must also be owned by the current user and offer no group or
other permissions). On Unix that mode is the note's protection, since the
daemon writes it 0600; on Windows the client checks only the regular file, its
size and the header, and the private home's inherited ACL is the boundary. It
requires the `dimcode-startup-failure v1 pid=<child pid>` header, removes
control characters other than newline and tab from the body, and returns it as
the command's error followed by `Details: <native home>/daemon.log`. The
storage gate writes the note on a diagnosed incompatibility and names the
store, its marker, the daemon's supported format, whether the store is newer,
mismatched, unrecognized or too old, and the recovery paths ([native storage format §
startup incompatibility](appserver/native-storage-format.md#启动不兼容诊断)).
Without a matching note the embedded server reports the existing
`App Server exited (<status>); see ...`, and a `--server` binary reports that
the flag must name a standalone `dim-app-server` binary. The failed launch
removes the note after reading; a successful connection removes any note. The
CLI never prints `daemon.log`, never deletes, recreates, downgrades or rewrites
the store, and exits 1.
Development documentation continues to use `./dev` and its isolated home; the
terminal note itself names a compatible build or `--home PATH`.

```text
$ dima
Error: Cannot open native store /Users/me/.dimcode-alpha/rust/dimcode.sqlite.
  Store format: v30
  Daemon format: v29
  This daemon cannot read the store. Existing data was preserved.
  Run a build supporting v30, or use --home PATH for a separate environment.
  Details: /Users/me/.dimcode-alpha/rust/daemon.log
```

The interactive export parser reads the format token and treats the remainder
of the line as a literal path, including spaces, without shell evaluation or
quote expansion. Missing paths and unsupported formats print usage and return
to the prompt without submitting input. `/exit` takes no arguments. Only these
exact command names are intercepted; other text follows the normal input path.
Subcommands do not create a plugin command registry.

### Migration interaction

`dim` and the alpha package's `dima` invoke these same native commands. Bare
`migration` imports each existing distribution independently:

| Distribution | Legacy source | Native target home |
| --- | --- | --- |
| Global | `~/.dim/v2` containing `dimglobal.sqlite` | `~/.dim` |
| CN | `~/.dimcode/v2` containing `dimcode.sqlite` | `~/.dimcode` |

When both exist, both are imported. Sessions, Providers, Settings and task ledgers
remain in their own target home. The default migration command uses these paths
independently of the chat client's alpha/development home or `DIMCODE_HOME`.
`./dev migration` invokes the same routing without injecting its development home
or restarting a daemon. Development verification uses disposable user homes.

An explicit `--home TARGET` scopes the command to that one home, reading
`TARGET/v2` when `--source` is absent. `--source OLD_HOME` selects just that
source; when it names a `v2` directory its parent is the target. Other custom
source layouts require `--home TARGET` to make the destination explicit.
The shared Migration plugin still receives one resolved source/target pair per
invocation and knows no distribution directory convention.

Default `list` reads both target homes. Default `show`, `retry` and `cancel`
locate the task through the owners' task lists, including when its old source
has been removed. JSON output is one object keyed by `global` and `cn`;
each entry contains `home` and `result`, or `home` and `error`. An absent
distribution is omitted. No matching source is an error; a task query with no
matching task is an error; listing with neither target home present returns `{}`.
A failure in one
distribution is reported while the other is attempted. Cancellation stops the
remaining imports. Overall exit is 1 for any failed distribution, otherwise 2
for any partial result, 0 for success, or 130 when cancelled. Explicitly scoped
commands retain their single-home JSON result.

Inspect, run and retry require stopped legacy writers. Run and retry also require
closed native clients and use normal verified shutdown to settle an existing target
daemon. Without `--source-stopped`, a terminal shows the resolved homes and asks
once for all selected sources. Declining exits 130; scripts must supply the flag.
Source/target overlap is rejected before target access. Migration performs the data
conversion offline under the native store lease; it does not start a daemon.
The CLI binds `TARGET/rust/dimcode.sqlite`, matching its normal profile. Queries
and inspection do not open that database. Disposal drains accepted work before
exit. There is no persistent Migration selection or automatic trigger. Imported
business configuration takes effect through normal feature loading.

Open imported Global data with `dim --home ~/.dim`; the ordinary default home
does not switch to Global. `dima` uses the same migration destinations, so pass
`--home ~/.dimcode-alpha` explicitly to import into the alpha home instead.

```text
$ ./dev migration
Source: /Users/me/.dim/v2
Target: /Users/me/.dim
Source: /Users/me/.dimcode/v2
Target: /Users/me/.dimcode
Close legacy and native clients using these homes. Continue? [y/N] y
<fixed progress panel; see Migration terminal presentation>
Migration completed
Task: <task-id>
Full report: dim migration show <task-id> --home /Users/me/.dim

$ dim migration inspect --home /tmp/dim-new --source /tmp/dim-old --source-stopped
{ "database": "dimcode.sqlite", "tables": { ... }, "issues": [] }

$ dim migration run --home /tmp/dim-new --source /tmp/dim-old --source-stopped --json
{ "task": { "id": "...", "status": "partial", ... }, "objects": [ ... ] }

$ dim migration show <task-id> --home /tmp/dim-new
$ dim migration retry <task-id> --home /tmp/dim-new --source-stopped --json
$ dim migration cancel <running-task-id> --home /tmp/dim-new
$ dim migration list --home /tmp/dim-new
```

The [Migration plugin](plugins/Migration.md#终端展示) owns the fixed terminal
progress panel and summary. Interactive run/retry displays that panel and finishes
with a compact summary and report command. `--json` suppresses progress and prints
the complete JSON on stdout; non-interactive output also remains JSON, with at most
one stderr notice per step. Queries keep their JSON results. Exit codes are 0 for complete, 2 for partial, 1 for failure and 130 for
settled cancellation. A partial report names failed, conflicting, deferred and
unsupported objects and omitted fields. Ctrl+C requests cancellation and waits
for accepted writes; a second process can use `cancel TASK`. Retry
requires the stopped acknowledgement again, accepts a moved source path and keeps
the prior report. Querying results does not require Migration to remain loaded.

Client preference replacement requires stopped clients. Existing values and
recorded clears remain unchanged. Ambiguous interrupted file writes are reported
as conflicts; retry does not replay them over later user changes.

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

For every export entry, relative output paths use the CLI process's current
directory, including after resume; neither the Session's cwd nor `--home` changes
that base. The CLI treats `~` literally. Usage/help states: `PATH is relative to
the CLI current directory; ~ is literal; use an absolute path for another base`.
An invoking shell may expand an argument before the CLI receives it.

Other target commands require their owning features. F6 does not implement a
fullscreen interface, persistent RPC/ACP server, verification/repair loop,
provider editor or Session deletion/fork. `dimcode export` is the
[Timeline export](appserver/timeline-export.md) (ADR 005 D3): every final model
request is a `model/call` line in it; the request snapshot filters of the
feature list are not implemented.

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
settlement, and exits with code 3. A [question](#questions) gets no answer
the same way: the question's branch and the turn are stopped, and the exit code is 3.
It does not change
`SessionConfig.interactive`. Sessions created as noninteractive retain Approval's existing owner-side policy (#98 D10).
`--permission` selects a legacy policy, including its file scope and risk decisions; a call that still asks is denied
the same way ([headless execution](#headless-execution)).
Other exit codes retain the native CLI contract, including 2 for turn failure
and 130 for interruption. A transport failure does not retry a submission.
Repeated `--image` arguments follow the headless attachment contract: text first,
then validated original image bytes in argument order, with paths relative to the
invoking process. An image-only invocation is valid. A submission error on a live
connection keeps the uncertain-outcome diagnostic/exit path unless its `error.data.appError`
is one of the seven [guaranteed pre-commit rejections](../../crates/dim-app-server/README.md#session-submission-errors):
`invalid_params`, `session_invalid_input`, `session_closed`, `session_build_failed`,
`service_reloading`, `unknown_method` or `input_blocked`. Those retain their ordinary error diagnostics.
Every other error, including `submission_unknown` and opaque publication/storage failures,
can follow commit and must not trigger another send.

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
return to the input prompt and never become model input. During an admitted
interactive export, Ctrl+C reports that the CLI is waiting for the result and
continues consuming further interrupts until the operation settles. Those
interrupts do not become an exit or draft-clear action at the next prompt.

## Permission controls

Permission owns the branch policy seeded by `session.create {permissionPolicy}` and changed with
versioned `session.permission.get/update`. The independent optional fields and legacy semantics
are the [Permission contract](../architecture/rust-agent/plugins/Permission.md#policy-contract),
extended by [ADR 017](../adr/017-independent-permission-controls.md). Command rules remain
`permission.rules`/`permission.set_rules` ([F2](appserver/permission-command-rules.md)); file grants
remain `session.grants.add/remove` ([ADR 012](../adr/012-session-grants.md)).

| TUI control | Choices | Policy field or owner |
| --- | --- | --- |
| File access | Read only / Workspace read/write / Unrestricted | `fileAccess` |
| Tool approval | Ask / Bypass | `toolApproval` |
| Process isolation | Off / Sandboxed · No network / Sandboxed · Network allowed | Existing `execution` settings, not Permission |

Ask means every tool call needs consent unless explicitly allowed by a tool or command rule;
Bypass skips tool questions only. Neither answers a file-scope question, lifts an explicit deny
or changes the sandbox. File access is a separate gate using the existing fixed facts: Read only
denies writes even for an allowed tool; read grants never permit writes. A call requiring both
scope and tool consent uses one approval. The new TUI never labels a choice Full access or Auto;
Auto is reserved for a future actual automatic decision system.

For compatibility, policies without explicit overrides retain the following legacy behavior,
after tool lists and command rules, with the existing file checks taking precedence:

| Preset | What runs (Permission's existing decision, after `disallowedTools` and deny rules) |
| --- | --- |
| `read-only` | Reads of the workspace, system roots, Session-owned files and granted paths run, and other reads ask; Session tools run; file writes, commands, network and unclassified tools are denied |
| `workspace-write` | Those reads run and other reads ask; workspace file writes and Session tools run; writes to system roots or Session-owned files are denied; other outside writes without a matching write grant, commands, network and unclassified tools ask |
| `full-access` | Every tool call runs without asking |

A deny rule applies under every preset. An allow rule only turns a question into a run for a
simple command; it never lifts a denial. The product and configured daemon profiles create
Sessions with `workspace-write`; that default is unchanged.

### Startup selection

`--permission read-only|workspace-write|full-access` applies to `dimcode`, `tui`, `exec` and
`exec resume`, before or after the command. Another value, a repeated flag or the flag on any
other command is an argument error before the daemon starts.

The terminal UI remembers independent `fileAccess` and `toolApproval` choices in the product
home's `config.json`, alongside legacy `permissionPreset`. Missing new fields remain missing;
loading old config does not convert a preset into explicit Ask or widen its legacy denials.
This is a CLI preference for later Sessions, not a daemon-wide default for all clients.
On the home screen without an active Session, `/permissions` saves the selected field without
sign-in or Session creation. The first message creates the Session with the remembered choices,
including within the same client process. With an active Session, remember the choice only after
its versioned `main` policy update succeeds. Changing one field preserves the other field,
the legacy seed and tool lists. Process isolation uses its existing host settings source instead.

```text
Home > /permissions > File access > Read only
  -> remember fileAccess (no Session or login required)
  -> first message -> session.create {permissionPolicy: {preset: <legacy seed>,
                                                       fileAccess: "read-only", ...}}
```

```text
$ dimcode exec --permission full-access -p 'Run the tests'
  hello
  session.create {cwd, permissionPolicy: {preset: "full-access"}}
  attach -> history -> submit -> idle -> detach

$ dimcode exec resume <session-id> --permission read-only -p 'Summarize'
  hello
  session.permission.get {sessionId, branchId: "main"}  -> {version: v, policy}
     preset already read-only: nothing is written
  session.permission.update {..., expectedVersion: v, policy: policy with preset read-only}
     permission_conflict: read again and retry, at most 3 attempts, then exit 1
  attach -> history -> submit -> idle -> detach
```

A new Session created with the legacy flag receives `{preset}` without explicit overrides.
The flag wins over remembered CLI choices for that run. New members inherit the requesting
branch's current policy (see branch semantics). A preset-only update on resume changes only
the legacy seed and retains all other policy fields, including explicit overrides and tool lists;
explicit overrides still take precedence over that seed. The notice must show their effective
values rather than suggest that changing a preset replaced explicit controls.
The update happens before attach, so no turn submitted by this invocation runs under the old
preset. Without the flag or remembered choices, `session.create` omits the policy and a resumed
Session keeps its own. Remembered choices are supplied explicitly when applied; they never
reinterpret stored history.
A resume flag against a daemon that does not advertise `session.permission.update` fails before
attach. A conflict retry is correct here because the requested preset does not depend on what
the read returned; the read only supplies the version and the tool lists to keep.

The effective controls are shown where the Session is identified. Interactive start reads the policy with
`session.permission.get` when the daemon advertises that method; `exec` prints it to stderr only
when `--permission` was given, so its existing diagnostics are otherwise unchanged. A
`permission/changed` event that arrives live on `main`, from this or another client such as a
paired device, prints one record with the independent values and any legacy tool exceptions.

The overview and Settings use `Ask (legacy tool exceptions)`. The compact TUI scrollback status
uses `Ask (legacy)` so saved-setting notices still fit 80 columns; this is the same legacy policy,
not explicit Ask or another option.

```text
$ dimcode --permission read-only
dimcode · model: provider/model
local files: /workspace/project
session: <session-id>
File access: Read only
Tool approval: Ask (legacy tool exceptions)

> Fix the build
Assistant: ...
[permission] main: File access = Workspace read/write  <- changed by another client
```

There is no status line: the scrollback client has no persistent area, and the editor's hint
line is fixed help text truncated to the terminal width.

### Headless execution

`exec` keeps its contract: an approval request is answered with the ordinary denial, the turn is
stopped and settled, and the exit code is 3. For legacy policies without explicit overrides,
`full-access` asks nothing except that explicit denies still refuse calls; `read-only` denies
writes and commands; `workspace-write` asks for commands and exits 3. Explicit Ask also asks for
otherwise in-scope reads; Bypass still leaves file-scope approvals intact. There is no automatic
approval answerer. The flag is an explicit legacy choice, so `exec` asks for no confirmation.

### Branch semantics

Permission's state for a branch is `SessionConfig.permissionPolicy` folded with the
`permission/changed` events of its ancestor prefix and then its own. The CLI writes only `main`,
the branch its permission controls target. Branch-scoped interaction follows
[Timeline branches](timeline-branches.md).

```text
session.create {permissionPolicy} --> SessionConfig.permissionPolicy
                                       |
                                       v
 main: permission/changed --> current branch policy
        |                      |
        | branch point         | sample when consuming a new member request
        v                      v
 child branch policy      new member Session policy
        |
        +-- its own member request --> sample child policy, not main's

Later updates do not reconfigure existing child branches or members.
```

- Child branches that the branch tool creates after a change inherit it. Existing child branches
  keep their policy; the CLI does not enumerate or update them. Their approval requests are
  still answered on their own branch id, as today.
- New Agent members copy the requesting branch's dispatched policy at the sampling point defined
  by [member permission inheritance](subagent-tool-approval.md#2-permission-inheritance). Existing
  members retain their policy, including on reuse. Permission-switch confirmation says that it
  affects future members created from this branch, not existing members. This is the target
  contract; delivery is tracked in #257.
- A fork copies the source's facts, so it starts with the source's current `main` policy (#26).

### Subagent tool approval

This section specifies the client interaction for [Collaboration's member-approval
route](subagent-tool-approval.md). It is a design requirement, not a claim that the route is
implemented. Member observation remains read-only ([member conversation](member-conversation.md));
answering uses the leader's approval surface and never submits directly to a member Session.

When the leader receives `approval.request` for `member_approval_allow`, the CLI queues it like
any other leader-Session approval, keyed by `(sessionId, branchId, requestId)`. Before enabling
Allow, call `session.collaboration.approval.get` with that leader request identity. Display the
returned server-owned member descriptor, not a command or explanation supplied by the leader
model. A non-main requesting branch is shown even if the user is currently viewing another line.

Scrollback and fullscreen use the same modal prompt and keyboard behavior:

```text
+-- Subagent tool approval ----------------------------------------+
| Requested by leader branch: research                             |
| Member: reviewer  Session: <member-session-id>                    |
| Member branch: main                                             |
| Tool: exec                                                      |
| Command: cargo test --workspace                                  |
| Member reason: Starting processes requires approval              |
| Leader reason: Allowing a member action requires approval        |
|                                                                |
| [Allow once]  [Reject]                                           |
| y allow once / n reject / Tab move / Enter confirm               |
+-----------------------------------------------------------------+
```

The line client presents the same fields followed by `Allow this member action once? [y/N]`.
Enter, another answer or EOF rejects under its normal approval rules. It does not offer `a`.
`Allow once` answers the **leader** request with `approval.respond allow`; Reject answers that
request with deny. Collaboration settles the original member request. Do not use the member id
from the displayed descriptor as the RPC answer target.

For original calls other than `exec`, show the original tool and its full input, prominently the
resolved `path`, URL or boundary (`sandbox`) where present. Commands, paths and ids remain verbatim;
localize labels only and sanitize terminal control characters. Wrap/scroll long detail and expose
all arguments before confirmation; do not silently truncate the action to a title or summary.
If the original command is simple, this wrapper still has no Always action: it is not that command
and must not create a command rule. No Full Access button or implicit policy update is offered in
this prompt; permission settings remain a separate user action.

| Client state | Display and action |
| --- | --- |
| Detail loading | Show `Loading member action...`; disable Allow; Reject remains available |
| Query failure/reloading | Show the error with Retry and Reject; keep Allow disabled and preserve the prompt |
| Method absent | Explain that this client/server combination cannot review the member action; offer Reject, never fall back to approving an opaque route id |
| Query returns `ended` | Close as `Member request ended`; deny the still-pending leader wrapper if necessary; no success message |
| Another leader request arrives | Queue it; do not replace the focused approval or reuse its loaded descriptor |
| `approval.resolved` / `approval.cancelled` from another client | Retire only the matching full request identity; a resolved answer is not evidence the member tool ran |
| Member cancellation while open | `session.notice` from Collaboration with kind `memberApprovalEnded` retires the matching branch/route prompt and denies the still-pending wrapper. Recheck detail on focus/accept as well |
| Leader stop / closing | Use normal stop/settlement; discard local detail state after its request ends |

Accepting Allow performs a fresh detail check before sending the answer. A request ending after
that check remains safe through server admission and route checks. The prompt's descriptor is
immutable; focus changes never change its target. A pending member request that ends between
checks is removed by the terminal notice, and cannot be approved onto another call. A terminal
notice racing a detail reply wins; an old pending reply never reopens that prompt. Reattachment rebuilds
from replayed leader approval requests and queries details again; cached pre-disconnect snapshots
are not grants.

An approval takes existing modal priority over member detail and task panels in both TUI modes;
closing the panel preserves the leader draft, selected model, selected line and transcript position.
Incoming requests do not move the user into the member Session. An adopted member ask awaiting the
leader model may be represented by its normal input/tool activity; it is not yet a user permission
prompt. Do not label the tool as approved merely because the user allowed the wrapper.

Headless `exec` keeps its existing deny/stop/exit-3 contract for a leader approval. `exec --json`
may emit the raw notifications, but adds no stdin approval protocol. Non-interactive leader Sessions
are settled by Collaboration without prompting. A terminal that loses its connection neither
approves nor cancels work by that fact alone.

### `/permissions`

`/permissions` is the visible menu entry in both TUI modes; `/approvals` and `/approval` open the
same controls as compatibility aliases, not a second preset chooser. The control applies to
`main`; each field selection writes only its dimension using the current whole policy and
version. A failed save neither changes remembered choices nor falls back to a legacy preset.

```text
Permissions (main branch)
> File access          Workspace read/write >
  Tool approval        Ask                  >
  Process isolation    Off                  >

File access                         Tool approval
  Read only                       > Ask
> Workspace read/write              Bypass
  Unrestricted

Hint: Read only: no writes.          Hint: Ask before tool calls.
      Tool approval still applies.        File restrictions still apply.

↑↓/Tab move · Enter apply · Esc back
```

In both fullscreen and scrollback, File access, Tool approval and Process isolation show short
explanations below all choices and actions, separated by a blank line. The block starts with
`Hint:`; each paragraph starts on a new line, wraps at word boundaries to the available width,
and aligns continuation lines after the prefix. Explanations are not selectable rows. Errors
remain distinct from these hints.

An old policy with no explicit tool override shows `Ask (legacy tool exceptions)` where
appropriate, with its existing automatic allows and risk denials explained rather than
pretending explicit Ask is active. Merely opening the panel writes nothing. Selecting Ask
explicitly replaces the legacy tool-risk behavior; selecting File access alone does not.
Increasing file access or choosing Bypass explains that explicit denies remain and process
isolation is unchanged; it does not claim every call will run. No combined permissive shortcut
appears in an approval prompt.

At the line prompt the no-argument command prints effective independent controls and numbered
command rules when supported. Preset arguments remain compatibility operations, not TUI labels.
The printed rules and revision are kept for `remove`.

```text
> /permissions
File access (main branch): Workspace read/write
Tool approval (main branch): Ask (legacy tool exceptions)
Process isolation: Off
Command rules (user scope, every Session):
  1  allow  exact   cargo test -p dim-cli
  2  deny   prefix  git push

> /permissions deny prefix git push
Rule already present: deny prefix `git push`

> /permissions remove 1
Rule removed: allow exact `cargo test -p dim-cli`
Command rules (user scope, every Session):
  1  deny   prefix  git push
```

| Form | Calls | Conflict |
| --- | --- | --- |
| `/permissions` | `session.permission.get` on `main`; `permission.rules`; existing execution settings read | none |
| File access / Tool approval selection | `get`, then `update` preserving untouched policy fields | Re-read before any retry; never replace the other dimension or tool lists |
| `/permissions PRESET` (legacy) | `get`; for `full-access` a `[y/N]` confirmation where Enter, any other answer, EOF and Ctrl+C mean No; then `update` with the read version and otherwise unchanged policy | `permission_conflict`: read again, print the current policy, write nothing; the user decides again |
| `/permissions allow\|deny exact\|prefix COMMAND` | `permission.rules`; unless the same rule exists, `set_rules(revision, rules + rule)`, then print the new list | read again and retry, at most 3 attempts: adding does not depend on the other rules |
| `/permissions remove N` | `set_rules(listing revision, listing without rule N)`, then print the new list | `permission_rules_conflict`: read again, print the current list, remove nothing |

`remove N` refers to the numbered list printed last. The listing, an added rule, a removal and a
removal conflict each print the resulting list, and only a printed list is kept for `remove`, so
a number never refers to a position the user has not seen. A form that prints no list (a rule
already present, a preset switch, an "always" answer during a turn) keeps the last printed one; if
the rules changed since, its revision makes the next `remove` fail, and the conflict prints the
current list. Without a printed list in this process, `remove` asks for `/permissions` first.
COMMAND is the rest of the line; the CLI rejects an empty command or one containing a separator
character (`dim_agent_types::COMMAND_SEPARATORS`, the set F2 validates) before any call. A
preset that is already in force writes nothing. Without the advertised methods, the command
reports that this daemon does not provide them. While a call is outstanding, Ctrl+C reports
that the CLI is waiting for the result, as for `/export`. Errors are printed as command errors
and never become model input.

### "Always allow"

The approval prompt offers `a` when all of these hold; otherwise it is `Allow once? [y/N]` as
before:

- the call is `exec` or `execute`, the two tools F2 names (the request carries no risk
  category; a rule for any other tool would never match, so the failure mode is asking again);
- `call.input.command` is a string with at least one word and no separator character;
- the daemon advertises `permission.rules` and `permission.set_rules`.

```text
[approval] exec: Tool `exec` requires approval for starting processes under the workspace-write preset
  command: cargo test -p dim-cli
Allow once, always, or deny? [y/a/N] a
Always allow `cargo test -p dim-cli` (exact). Press Enter, or type its leading words for a prefix rule: cargo test
[rule saved] allow prefix `cargo test`
[tool] exec
[done] exec
```

`y`/`yes` allows once and `a`/`always` asks for the rule; every other answer, including an
empty line, denies. At the rule prompt Enter keeps the exact rule; the command's leading words
make a prefix rule (all its words make the exact rule); other text repeats the rule prompt. EOF
and Ctrl+C keep their meaning: EOF denies and closes input, Ctrl+C stops the turn.

```text
 CLI                                    App Server
  | approval.request {exec, command}  <--|
  | user: a, rule words                  |
  | permission.rules ------------------->|  {revision r1, rules}
  | permission.set_rules(r1, rules+rule)->|  {revision r2}
  |    permission_rules_conflict: read again and retry (at most 3 attempts)
  |    other failure: [rule not saved] ..., then "Allow once? [y/N]" for the same request
  | approval.respond allow ------------->|  Approval rechecks; the rule now answers
  |                                      |  identical later calls without asking
```

The rule and the answer are separate writes. A rule that already exists is not written again
(`[rule present]`). If the request is cancelled while the write is outstanding, the CLI reports
the rule's outcome and sends no answer. After Ctrl+C it starts no further write: a pending read
ends as `[rule not saved]`, and the stop settles the request. The recheck after allow (F2 §5.3)
still applies, so a deny rule written meanwhile wins. Command text is shown through the terminal
filter.

### "Allow for this session"

For `read`, `write` and `edit` calls with a `path`, a file-scope question also offers `Allow for
this session` (ADR 012). With Read only or Workspace read/write file access, an outside read
needs scope consent. An in-scope call asking only because of explicit Tool approval Ask does
not need a file grant. The CLI sends the tool's path
unchanged to `session.grants.add {sessionId, path, kind: "file", mode}`: the mode is `read` for
`read` and `write` otherwise. The daemon resolves `~`, a relative path, `..` and symlinks against
the Session workspace and stores the canonical path; a new file is resolved by its parent and
final name. The CLI decides no path or kind locally.

```text
[approval] read: Reading outside the session workspace requires approval: /home/user/.ssh/config
  path: /home/user/.ssh/config
› Allow once   Allow for this session   Reject
```

`Allow for this session` writes the grant first, then answers `approval.respond allow`; the
recheck sees the grant. If the write fails, the CLI shows `Grant not saved: <error>` and still
allows the call once, as the user asked, so no failed store write blocks the turn. A read
outside the facts in a non-interactive run (`exec`) is denied with the ordinary refusal and exit
code 3; no new error path is added. This prompt does not list or remove grants; revocation has
no client surface yet. A grant answers file scope, not explicit Tool approval Ask; the current
answer still consents to this call once. A write grant also admits a read, a `read` grant never
authorizes a write, and a fork starts without the source's grants.

### Decisions

1. **Remembered CLI choices are not a host default.** Independent choices are saved alongside
   old config and sent explicitly by the CLI. A default honored by Web, ACP, paired devices and
   automation belongs to the daemon's user Settings: requirement R1. Do not reinterpret old
   config or existing policies to simulate such a default.
2. **No `--allowed-tools`/`--disallowed-tools`.** Tool names belong to the daemon profile and the
   protocol offers no catalog to validate them, so a misspelled name would be accepted and do
   nothing. `allowedTools` also bypasses the preset for every call of a tool, which presets and
   command rules already express more narrowly. The CLI keeps existing lists on every update.
3. **The CLI writes `main` only** (branch semantics above).
4. **No Shift+Tab cycle.** The key decoder belongs to the idle draft editor, which returns only
   send or exit; a running turn reads plain lines. A cycle needs an editor action result and a
   separate interaction design. `/permissions` covers switching.
5. **`/permissions` also adds rules.** F2 §8 lists adding as part of the panel, and with product
   storage the rules live in a database row that the user cannot edit as a file; without `add`
   a deny rule could not be created from the CLI.
6. **Retry only when the outcome does not depend on the read.** A startup preset, an independent
   field assignment and adding a rule can retry after re-reading and preserving untouched fields.
   Legacy panel preset switching and removal by number report conflicts because the user chose
   from what was shown. Execution Settings retains its inspected-revision save contract.
7. **Legacy permissive preset confirmation remains.** An interactive legacy `full-access`
   argument still requires confirmation; the startup flag is itself explicit and works headless.
   New controls describe their independent effects rather than offering that combined TUI choice.

### Out of scope

| Item | What it needs |
| --- | --- |
| A reviewer model that answers approvals for the user | An approval answerer that responds through Approval's public respond operation, with its own model call and design, like the Collaboration leader route; not a CLI change |
| Shift+Tab cycle | CLI only: an editor action result and a confirmation path (decision 4) |
| `--allowed-tools`/`--disallowed-tools` | A tool catalog read to validate names at the argument boundary (decision 2) |
| `/permissions` during a running turn | CLI only: the running turn reads plain lines and answers approvals; panel calls would join its multiplexed loop |

ACP, Hooks and Web remain legacy/preset-only clients for permission selection in this change.
Their existing preset controls do not select explicit File access or Tool approval, and their
omitted overrides retain legacy risk decisions. Extending those UIs is separate work, not a
second implementation of the TUI controls; Web's existing execution settings remain host settings.

### Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (bounded plugins, App Server only assembles) | No new plugin or Capability; the CLI is a client of existing methods |
| Principles §3 (dependency direction) | The CLI depends on `dim-agent-types` for `PermissionPolicy`, `CommandRule` and `COMMAND_SEPARATORS`, as it already does for events; nothing depends on the CLI |
| §4 rule 1 (command events) | Policy changes go through Permission's existing `session.permission.update`, which writes `permission/changed`; rules and grants go through their owners' typed writes |
| §4 rule 2 (handover) | Unchanged |
| §4 rule 3 (before callback) | Not used |
| §4 rule 4 (core has no business meaning) | Timeline and Dispatcher are unchanged; independent policy semantics belong to Permission, not the CLI |
| §4 rule 5 (batches) | The rule write and the approval answer are separate owners' writes, ordered by the client as F2 §5.4 specifies |
| §4 rule 6 (context) | The model sees only the existing results and denials |
| §4 rule 7 (public interfaces only) | Only public methods and notifications; the CLI reads `permission/changed` from the public event stream |
| §4 rule 8 (infrastructure unchanged) | No change; two requirements are raised below for App Server owners, not infrastructure |
| §4 rule 9 (only needed facts in Timeline) | No new event; rules and grants stay out of the Timeline |
| ADR 002 D1 to D5 | No new part, wiring or dependency; additive policy fields follow ADR 017, CLI preferences are local business configuration |
| ADR 003 | Presets and rules govern only agent tool calls; `!command` is unaffected; rules are user scope only, so no project file can widen them; replay, restore and fork confer nothing new |

### Requirements raised to mechanism owners

**R1. A user default preset for new Sessions.**
- Blocked: the feature list's permission policies and tool-approval settings need a default
  that new Sessions use. `session.create` takes its default from the daemon profile's fixed
  `session_defaults` (`workspace-write` for product and configured profiles), and no Settings
  source holds a user choice.
- Considered: a CLI file or environment variable (only `dimcode` would honor it; other clients
  and automation would create Sessions with another default); a field in the `permission`
  source (right owner, but it changes the F2 document and `session.create`, a server change).
- Needed: a user-scope value, never project scope, that `session.create` uses when the request
  omits `permissionPolicy`, managed by local-only revisioned methods like `permission.rules`.
  An existing Session never reads it again: its `SessionConfig` is fixed at creation.
- Ordering, batches, interruption, reload: none; it is read once at creation and is not an event.
- Owner: App Server (Sessions service and the `permission` Settings owner); tracked in
  [#257](https://github.com/arcships/dim/issues/257).

**R2. Agent members follow the leader branch's current policy.**
- Owner: Collaboration; tracked in [#257](https://github.com/arcships/dim/issues/257).
- Contract, sampling order, failure and reuse behavior are owned by
  [member permission inheritance](subagent-tool-approval.md#2-permission-inheritance).
- CLI permission controls update their target branch only. They do not enumerate members or
  propagate policy. Their confirmation explains the consequence stated under Branch semantics.

## Questions

The model's `ask_user` tool asks the user one to four multiple-choice questions and waits for
the answer, which becomes the call's tool result
([Questions](../architecture/rust-agent/plugins/Questions.md), #262). The line-mode CLI is one of
the clients that answer. It asks the waiting calls of every branch of the attached Session, one
call at a time in arrival order, and answers with `session.question.respond`:

```text
> Plan the service
[question] Database                             <- header, or "[question]"
1. Which database should the service use?
   1. PostgreSQL  managed, relational
   2. SQLite      single file
   0. Other (type your answer)                  <- only where custom text is allowed
Choose 1-2, 0 for other, empty to decline: 1
2. Which features ship first?                   <- multiSelect
   1. Auth
   2. Billing
   3. Search
   0. Other (type your answer)
Choose numbers, comma separated, 0 for other: 1,3,0
Other: audit log
[question answered]
[question] Deploy (branch reviewer)             <- a question of another branch
1. Where to deploy?
   1. production
   2. staging
   0. Other (type your answer)
Choose 1-2, 0 for other, empty to decline:
[question answered elsewhere]                   <- another client answered first
```

The CLI finds waiting calls in the `step/tool-call` and `step/tool-result` events of each branch:
live `session.event` notices, `main`'s history read at attach, and the history of every other
branch, read once from its first `session.state` notice. The same event can arrive live and from
history, in either order, so calls and results are matched by event identity. A call id may
come back in a later model request, so a call is named, settled and answered by its branch, the
`request_id` of its `step/tool-call` and its call id:

```text
session.event (live, every branch) ─┐
session.history (main at attach,    ├──> waiting ask_user calls ──> one prompt at a time
  other branches on first state)  ──┘    step/tool-call adds a call      │
                                         step/tool-result removes it     v
                                                          session.question.respond
                                                          {branchId, requestId, callId,
                                                           answers | declined}
```

An empty line on the first question declines the whole call; an empty or invalid line on a later
question asks it again. While a question is open, approvals, model output and shell results wait,
as they do for an approval prompt, and a question waits for an open approval.

| Reply or event | CLI |
| --- | --- |
| `{}` | `[question answered]` |
| `invalid_params` | `[question rejected] <reason>`; the call is asked again from its first question |
| `question_not_pending` before the call's result | `[question not ready; press Enter to send again]`; Enter sends the same answer |
| Another failure before the call's result | `[question error] <error>; press Enter to send again` |
| The call's `step/tool-result` before the reply, or while nothing is sent | `[question answered elsewhere]` for an answer or decline, otherwise `[question cancelled]` |
| `Ctrl+C` or EOF at a question | `session.stop` for the question's branch, which ends its other calls too, and for the turn while it runs; as for an approval prompt, a running shell command is not stopped first |

Without terminal input (`exec`, or a redirected prompt) nobody can answer. The CLI prints
`Interactive answer required; stopping. Rerun in a terminal to answer it` once, stops the branch
of every question it reads before it exits and the turn while it runs, and exits with 3. Exit code
3 now means "interactive approval or answer required".

### Terminal UI questions

The terminal UI uses the existing Questions client module and prompt rather than
a second waiter projection or protocol. It reads committed calls/results on every
branch through the existing fixed-watermark history/live merge. The call is always
addressed by its original Session and `(branchId, requestId, callId)`.

```text
 [ Database ] [ Features ] [ Confirm ]
 Question on branch reviewer
 Which features ship first?
   [x] Auth
   [ ] Billing
       Type your own answer: audit log
 Space toggle · Enter next · Tab switch · / commands · Esc decline
```

Reuse the current prompt's selection, review and narrow-screen window: number keys
choose, Space toggles, Tab/Shift+Tab or Left/Right moves between questions, and Enter
on Confirm submits. Custom text is available only with `allowCustom`; it is sent
separately from offered labels. Non-empty text on the custom row checks it as it
is typed (replacing the chosen label in a single choice) and clearing it unchecks it;
while editing, Esc leaves the editor keeping the text and Enter keeps it and moves to
the next question. Outside the editor, Esc declines the whole call. Display sanitization
never changes the labels sent to the owner. The composer's draft is preserved.

A pending question does not block commands. While its free-text answer is not being
edited, `/`, Ctrl+O and Ctrl+P open the command palette over it, drawn where that mode
draws the palette, and the question keeps its tabs, selections and typed answers. The
palette opens command entry, which keeps the keys through the command's argument, shown
on the composer line; Enter runs the command as it runs during a reply, and command
entry lasts until that Enter, Esc, or a draft that stops being a command. Esc in the
palette and while an argument is typed closes command entry and gives the keys back to
the question, declining nothing and interrupting no run. A command that opens its own
panel puts the question aside and brings it back, with its state, when that panel
closes; a question the Session stopped asking meanwhile is not brought back. `/` is the
palette until the free-text row is edited, so an answer may itself start with a path:
Enter, Space, the row's number or any other typed character edits the row, and `/`
typed there is text.
Esc keeps its meaning on every other surface: it declines an open question and
interrupts a busy run with no prompt open, with no timing window.

While submitting, disable duplicate responses. The successful reply or a committed
batch result closes the prompt; invalid/not-pending/other failures keep the answers
with a diagnostic and an explicit Confirm retry (Esc retries a decline). A result before a late reply wins;
neither a reused call ID nor another call in the same model request reuses old UI
state. Approval/question prompt order follows the existing modal behavior; a leader
question closes a member detail through its existing cleanup, and a member's question
never becomes a leader response control. Ctrl+C follows the existing TUI interrupt/
exit policy; when it interrupts, address the question's branch and any running leader
turn before a separate shell run. Line-mode and no-terminal policies are unchanged.

The [CLI contract](../../crates/dim-cli/README.md#questions-in-the-terminal-ui)
records the delivered interaction. Acceptance exercises the actual TUI and NDJSON
boundary for structured answers, retry, reused identities, branch restoration,
member isolation and draft preservation; an actual App Server/model flow verifies
that the next model request reads the answer through `ToolResult.content`.

## Implementation and verification

Changes stay in `crates/dim-cli`: `args.rs`/`main.rs` select commands,
`session.rs` reuses Session execution and its shared paged-history reader,
`display.rs` keeps human rendering separate from JSON output, and `export.rs`
formats conversations and writes new files. `permission.rs` holds the permission operations any
CLI client can call, without terminal input or output: reading and updating the `main` policy
with version checks, listing, adding and removing command rules with revision checks, and the
"always allow" steps (the eligible command, the proposed rule, the rule write as request and reply
steps for a turn loop that multiplexes its connection, and the allow answer).
`permission_command.rs` is the line-mode `/permissions` command, and the approval prompt stays in
`session.rs`; both only show results. `question/mod.rs` holds what any client needs for questions,
without terminal input or output: the `ask_user` input rules, the waiting calls derived from
events, the `session.question.respond` params and what a reply means. `question/line.rs` is the
line-mode prompt that `session.rs` feeds with notices and replies. Conversation formatting belongs
to the CLI. Independent controls use Permission's additive policy contract (ADR 017) and the
existing execution Settings contract; no core lifecycle, storage migration or runtime registration
change is required.

Reuse `cli_flow.rs` and its actual daemon profile fixture to cover `exec` and
resume, stdout/stderr separation, public JSON notification preservation,
turn/approval failure and stop settlement, fixed-watermark multi-page export,
both formats, existing-file rejection, and interactive command interception.
Cover relative paths when the CLI and Session directories differ, literal `~`
usage, child-branch omission/markers, failed or cancelled child creation, and
Unix file privacy.
Use isolated homes only. Implementation requires the focused CLI flows and
affected Clippy.

Verify permission controls at both boundaries. Reuse scripted daemon flows for the
`session.create` policy, the resume read/update with a conflict retry, argument errors, the
`/permissions` listing, legacy aliases and confirmation, conflict reports, adding and removing rules with the
new numbered list printed after each, two removals in a row where the second number refers to the
list the first printed, the "always" prompt with a prefix rule, a conflict retry, a failed write
that falls back to answering once, an existing rule that is not written again, Ctrl+C during the
write that abandons the rule, and a command with a separator that is not offered "always". Flows against the built App
Server (run by CI with `DIM_TEST_APP_SERVER`) should check that `exec --permission full-access` runs a
process tool without a question, that default `exec` still exits 3, that `exec resume
--permission` changes the resumed branch, and that "always" writes an exact rule after which the
identical call runs without asking, until `/permissions remove` deletes it.

The independent-control acceptance cases are: an in-scope read asks with explicit Ask; a tool
allow answers only the tool question; Bypass still asks outside file scope; Read only denies a
write even with a tool allow; one call needing scope and tool consent prompts once; a read grant
cannot authorize a write; an explicit deny wins with Bypass; old policies/config retain exact
legacy behavior without silent writes. Verify each choice changes only its own field, remembers
it only after a successful policy update, works before Session creation, and reports conflicts or
unsupported servers without widening. Check both TUI modes show only `/permissions` in the menu,
both aliases work, and Process isolation has no duplicate Execution/Sandbox entry. Reuse process
flows for the boolean mapping, CAS failure, unavailable-backend reason, and a process started
before an isolation change retaining its old boundary. These are requirements, not a claim of
implementation or verification completion.

Questions are covered by scripted daemon flows (`cli_flow/question.rs`): single and multiple
choices, custom text, a decline, invalid lines, inputs the handler rejects and other tools that
ask nothing, deferral in both directions with an approval, a shell result that waits, a call id
used again, a call id that a later request reuses arriving live before the history page holding
the earlier call and its result, a child-branch question read from two history pages and answered
with its `branchId`, a duplicate live event, every reply in the table above, results that close
the prompt before or during the reply, Ctrl+C and EOF before and after the turn settles, a reply
drained after Ctrl+C, and `exec` exiting 3 for questions on `main`, on a child branch and after
the turn settled, while a result read before its call asks nothing.
