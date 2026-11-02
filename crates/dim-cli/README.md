# Rust CLI

`dimcode` is a thin Rust client for the local App Server. It owns command-line input, scrollback output, approvals, questions and account prompts. The App Server owns model execution, tools, session state and persistence. The npm wrapper exposes the same program as `dim` and `dimcode`; the alpha wrapper exposes `dima`.

`migration` and `./dev migration` import Global `.dim/v2` into `.dim` and CN
`.dimcode/v2` into `.dimcode` independently, processing both when present. Default
task queries search both homes. A terminal asks once to confirm writers have stopped. Run/retry settle the native
daemon through normal shutdown and write its existing `rust/` store offline. `--home` scopes the command to one target (`HOME/v2` by default),
`--source` overrides its source, and `--source-stopped` acknowledges stopped
writers for scripts. Development verification uses disposable user homes.
`migration inspect/run/list/show/retry/cancel` mounts the shared Migration plugin
only for that invocation. Source checks, stopped-source acknowledgement, progress,
JSON reports, cancellation and exit codes follow the [migration command contract](../../docs/design/cli-commands.md#migration-interaction).

`dimcode tui` opens the interactive client. `dimcode exec -p TEXT` runs one prompt
and exits; redirected stdin supplies the prompt when `-p` is absent.
`dimcode exec resume SESSION_ID -p TEXT` continues a persisted Session. Existing
`--prompt`, piped stdin and `--resume` entries remain available.

`dimcode acp [--agent ID]` mounts the reusable [ACP Gateway](../dim-acp/README.md)
over stdin/stdout, using the same home, server and model-profile options. It has
no terminal UI; stdout contains ACP frames. Supported methods, lifecycle and
client limitations are in the [ACP contract](../../docs/design/acp-gateway-implementation.md).

`exec --json` writes one public JSON-RPC notification per line for the selected
Session. Session handles and diagnostics go to stderr. The stream preserves
public fields, includes live observations after attach and does not replay stored
history. Approval is denied, the turn is stopped and settled, and exec exits 3.
A [question](#questions) is not answered: its branch and the turn are stopped, and exec exits 3.
`--permission` (below) only changes which calls ask; nothing is approved automatically.
Ctrl+C waits for settlement and exits 130. Further interrupts while settling
keep waiting; they do not carry over to the next interactive prompt.

`exec --image PATH` adds a local PNG, JPEG, WebP or GIF; repeat it for several
images or omit text for an image-only request. Empty or whitespace-only text is
omitted when images are present; other text is preserved exactly and comes first.
Images retain argument order. Without `-p`, redirected stdin is read through EOF,
including for image-only requests. Relative image paths use the invoking
process's cwd, including with `--cwd` or `exec resume`; image URLs are not fetched.

```text
$ dimcode exec --image before.png --image after.png -p 'Compare these images'
$ dimcode exec resume <session-id> --image screenshot.png --json > run.jsonl
$ printf 'Describe this\n' | dimcode exec --image screenshot.png

local files -> shared image scaling -> user.content -> session.submit
```

The CLI reads, scales and checks all local images before connecting with the same
rule `read` uses, written once in
[WorkspaceTools § read results and images](../../docs/architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images):
an image is made acceptable within that rule's longest-edge and encoded-size
limits (for example 2048 px and 5,000,000 bytes of base64 today), a PNG kept while its scaled PNG fits
and JPEG otherwise, the EXIF orientation applied, and an animation sent as its
first frame. The only size refusal is that rule's pixel guard, which names the
dimensions; invalid or truncated data and unsupported formats fail. Format comes
from bytes, including when a filename has another extension. The `image` dependency
enables the four input formats; TIFF and BMP are decoded only as macOS and Windows
clipboard bitmaps. Local symbolic links are followed, and file type is checked on
the opened file. At most 20 images are accepted. The combined base64 size is
checked before connecting; the final encoded request, including its JSON envelope
and text, must still fit 16 MiB. An image whose sent form differs from its file
adds one line to stderr before the run, as in
`Scaled photo.jpg from 8064×6048 to 2048×1536 px (image/jpeg)` or, when only the
encoding changed,
`Re-encoded shot.png as image/jpeg (1500×1500 px)`; an animation says only its
first frame is sent. stdout carries only the reply or the JSON stream.

An error reply to `session.submit` can follow an already queued input. The CLI
reports uncertainty for publication failures and unclassified errors and exits
without resending, even while the connection stays open. Known pre-commit input,
Session, service and method rejections report their original error. Resume the
printed Session handle to inspect an uncertain outcome.

The idle prompt edits a multiline draft. Enter sends; Ctrl+J inserts a newline.
Terminal bracketed paste inserts one group without submitting embedded newlines.
Multiline pastes and pastes over 16 KiB appear as atomic markers:

```text
> Explain [Paste 1: 120 lines]
  and compare with the current file.
Enter: send  Ctrl+J: newline  Ctrl+V: clipboard  Ctrl+C: clear
Ctrl+C twice: cancel paste
```

Arrows move by grapheme or display row; Home/End select the logical line's ends.
Backspace/Delete remove an entire marker. Ctrl+C clears a nonempty idle draft;
with an empty draft it exits. An EOF key exits only when the draft is empty;
otherwise the draft remains editable. Ctrl+Z is an EOF key only on Windows.
If a paste never delivers its closing delimiter, press Ctrl+C twice to discard
that paste while keeping the preceding draft. That consecutive pair is reserved
inside bracketed paste; a single Ctrl+C byte remains literal paste content.
Paste bytes are not newline-normalized. Line labels follow Rust str::lines():
standalone CR is retained but is not a line separator.
An external interrupt discards the current paste and clears the idle draft.
Text wraps
and scrolls with the terminal size, and draft control characters are escaped for
display while the original text is retained. A terminal reporting zero geometry
uses 80 columns and 24 rows for the missing dimensions.

Pastes up to 16 KiB expand unchanged into input. Larger pastes, up to 10 MiB of
UTF-8 each, call the existing attachment.put only on Enter. Confirmed paths are
reused with the unchanged item after another put or local request encoding fails.
Those errors keep the draft editable. Ctrl+C during a sent put waits for its
reply, retains a confirmed path and sends no further put or model input.
Connection loss or an uncertain submission reply exits without retrying.
Recognized commands require plain single-line input; attached command drafts are
retained before any put or execution.
An ordinary single-line terminal paste is plain text, so a pasted command runs
only after the user presses Enter. Text resembling a stored-paste reference is
ordinary user text, not an authenticated attachment handle.

Type `@` and a local path to see matching entries. Up/Down selects a suggestion;
Tab enters a directory or inserts a file, and Esc closes suggestions without changing
the draft. The displayed `local files` directory is the CLI's selected `--cwd`, including
after resume. Paths outside that directory remain usable as explicit selections.

```text
local files: /work/project
> Compare @docs/
* guide.md
  reference/
Tab: select or enter directory   Esc: close suggestions

> Compare [Image 1: screen.png] with [File: guide.md]
Enter: send  Ctrl+J: newline  Ctrl+V: clipboard  Ctrl+C: clear
```

A paste containing only existing file paths inserts files. Quotes, escaped spaces and
local file URLs work; prose and HTTP links remain text. Supported image bytes produce
image markers even with another filename extension. Other ordinary files produce path
references, which the model can read with its existing tool. Image data is captured at
insertion; removing a marker never deletes the original file. A rejected file group
leaves the draft unchanged. File references retain separate ordered text content blocks.
An image whose sent form differs gives both sizes when the size changed,
`[Image 1: screen.png · 8064×6048 → 2048×1536]`, both media types when the re-encode
changed the type, `[Image 1: screen.png · image/png → image/jpeg]`, ` · first frame` for an
animation, and ` · re-encoded` when the re-encode kept both the size and the type; an image
sent as it is keeps the plain marker.
A path attached while the model takes images is read and scaled at insertion,
and a model that takes no images still receives the link.

Ctrl+V reaching the CLI reads clipboard files first, then an image, then plain text.
macOS uses AppKit and resolves native file-reference URLs to real paths before image
classification or file linking; Windows uses Win32. Linux needs `wl-paste` on Wayland or `xclip` on
X11. Missing or failed clipboard access reports an inline error and retains the draft.
The CLI reads only on that action and never changes the clipboard. A terminal that
intercepts Ctrl+V uses its ordinary paste event instead. Linux reads remain pending while
editing continues; Enter asks you to wait for insertion, and Ctrl+C clears the draft and
cancels the reader. macOS and Windows read and convert clipboard data synchronously;
editing and cancellation resume after that read returns.
See the [attachment input contract](../../docs/design/cli-attachment-input.md) for native
acceptance evidence; cross-compilation alone does not establish platform behavior.

Use `dimcode session export SESSION_ID --format markdown|json [--output PATH]`
for a fixed prefix of main-branch history. Without `--output`, it writes to stdout.
Exports include main only; Markdown labels child branch requests and failures
without fetching child histories. Files are created new, never overwritten, and
use mode 0600 on Unix or the inherited ACL on Windows. A write error can leave a
partial new file. Relative paths use the CLI process cwd; `~` is literal.

`dimcode session rename SESSION TITLE` sets the Session's manual title with
`session.rename` and prints the title the server returns;
`dimcode session rename SESSION --clear` sends `title: null` and prints that the
Session is untitled. Exactly one of TITLE and `--clear` is required, so a
forgotten or blank argument is a usage error and never clears a title. The method is
Session-scoped, so `hello.features` does not list it: the client calls it directly,
and a Session that does not serve it answers `unknown_method`, reported as an
unsupported daemon.

`dimcode session delete SESSION` permanently deletes the Session through
`session.delete`, and `dimcode session cleanup (--older-than DURATION | --keep N)
[--yes]` deletes the Sessions a selector matches. The delete reply says whether the
call removed the Session: `true` prints `Deleted ID`; `false` prints `Nothing was
removed: no stored Session ID.` on stderr and exits 1,
and a reply without the field, from an older App Server, prints `Deleted ID`. Cleanup
selects only the Sessions `/resume` lists by default
([classification](../../docs/design/appserver/session-classification.md)): origin
`client` or `unknown`, or a daemon that records none; an `internal` or `acp` Session is
never selected, does not count toward `--keep N` and does not appear in the preview.
It reads `session.summaries` with `includeClosed: true`, so a Session this daemon closed
is listed and marked `closed`; an older App Server refuses the field `invalid_params`,
and cleanup then stops before any delete with
`This App Server is too old to clean up Sessions safely; nothing was deleted.` and exits
1. It orders and filters by `updatedAt` (the last activity), and prints its selection
before anything is deleted: the id, the title and how long ago the Session was active,
one tab-separated line each. A Session with a running turn or an attached client is
kept, listed after the selection under `Kept (active):` with its reason, and `--keep N`
keeps it without its consuming one of the N; a failed or closing line is not a reason
to keep one, so such a Session is deleted when nothing else uses it. Every delete
carries `requireInactive: true`, so the daemon checks the Session again at the delete; a
`session_active` refusal is listed under `Kept (in use at the delete):` after the
deletes with the daemon's reason and is not a failure, and a `deleted: false` reply
prints `Already gone: ID` and counts as neither. When nothing was deleted because every
matching Session is active, the command prints `Nothing to delete: every matching
Session is active.` and exits 0; a selector that names no Session prints what it kept,
`Nothing to delete: --keep N keeps every Session.` or `Nothing to delete: no Session is
older than DURATION.`. On a terminal only `y` or `yes` confirms and `--yes` skips
the prompt; without a terminal nothing confirms, so a script passes `--yes`. A
confirmed run deletes every selected id: each failure is printed with its id and the
method's error, `dimcode session delete ID` is printed to retry it, and the run exits
1. `DURATION` is a count with a unit `d`, `h` or `m`.

Use `dimcode export SESSION_ID [--output PATH] [--database PATH]` for a Session's
whole Timeline ([ADR 005](../../docs/adr/005-observability.md) D3): every event of
every branch with its before trace, as versioned JSONL. It runs the App Server binary
(the sibling or `--server PATH`) in a read-only export mode and starts no daemon, so it
reads the home while a daemon runs or after `dimcode daemon stop`. The database is
`rust/dimcode.sqlite` under the home unless `--database` names a copy made with SQLite's
backup API (`sqlite3 dimcode.sqlite ".backup copy.sqlite"`) or after `daemon stop`, with
any `-wal` file beside it. The output is sensitive: it holds prompts, code and tool
output. Provider, credential and settings records are never read, but a key a user or
tool wrote into the Timeline is exported with it. Files follow the rules above, and a
failed export removes the file it created. See the
[Timeline export design](../../docs/design/appserver/timeline-export.md).

Use `dimcode daemon vacuum [--database PATH]` after `dimcode daemon stop` to
reclaim the space deleted Sessions leave in the native store. It runs the App
Server binary (the sibling or `--server PATH`) with `--vacuum`, takes the store's
maintenance lock, checks the current format, rewrites the database with SQLite
`VACUUM` and prints its size before and after. It refuses while a daemon owns
the database, never creates a database or upgrades one, and needs free disk space
about the store's size. The database path follows `dimcode export`. See the
[storage format](../../docs/design/appserver/native-storage-format.md#sqlite-运行参数与空间回收).

At the idle interactive prompt:

```text
> /export markdown Session notes.md
Exported Markdown: Session notes.md
> /export json events.json
Exported JSON: events.json
> /export json slow.json
^C
Waiting for export result before accepting more input.
Exported JSON: slow.json
> /exit
$
```

`/export` reads the format and treats the remaining path literally, including
spaces. Ctrl+C during an admitted export waits for its result; further interrupts
keep waiting and do not exit the next prompt. Configuration commands use the same
settlement rule. Errors return to the prompt. `/exit` takes no arguments. Slash commands
in exec or redirected input remain literal model input.
See [CLI commands and exports](../../docs/design/cli-commands.md).

At the idle interactive prompt, `/effort VALUE` (alias `/reasoning`) changes
reasoning effort, `/fast on|off` selects priority/default service tiers, and
`/agent ID` adopts an Agent profile's prompt and optional model. Tools and
permissions keep the current Session configuration. A profile with a model resets effort
and service tier to that model's defaults; a prompt-only profile preserves them. Bare commands list choices;
unsupported values leave the prompt usable. Commands in `--prompt` or piped input
remain literal model input. See [model configuration](../../docs/design/model-configuration.md).

```text
> /effort high
Reasoning effort configuration adopted: high
> /fast off
Service tier configuration adopted: off
> /agent <agent-id>
Agent prompt/model configuration adopted; Session tools unchanged.
> _
```

Ctrl+C during a configuration command waits for its result before accepting
another line, because adoption may already have committed. The client does not
automatically retry uncertain commands.

Build both native programs from the repository root:

```bash
cargo build --locked -p dimcode
target/debug/dimcode --help
```

Help and version commands do not start a daemon or read credentials. This crate is a library; the `dimcode` crate builds the single executable, which runs the CLI and, as `dimcode app-server`, the App Server. The CLI starts its daemon by running its own executable with `app-server`, through a `dimcode-server` link to it in `<home>/rust` so that process lists (`ps`, `pgrep dimcode-server`, Activity Monitor) tell the daemon from the clients that share the executable; `--server PATH` names a standalone `dim-app-server` binary, which the CLI runs by its own name, while the default instead runs this executable's `app-server` subcommand. Naming this executable is an error: it serves the App Server only behind that subcommand; a path that does not exist, a program that exits before serving, or one that does not start serving in time is reported with the same requirement.

## API-key configuration

Save this model configuration as `model.json`, replace the model ID, and set the referenced environment variable before starting the daemon:

```json
{
  "defaultModel": { "providerId": "openai", "modelId": "your-model-id" },
  "providers": [
    {
      "id": "openai",
      "kind": "openai-chat",
      "apiKeyEnv": "OPENAI_API_KEY",
      "baseUrl": "https://api.openai.com/v1",
      "models": [
        {
          "id": "your-model-id",
          "contextWindow": 128000,
          "maxOutputTokens": 16384,
          "config": { "maxTokens": 4096 }
        }
      ]
    }
  ]
}
```

```bash
export OPENAI_API_KEY='your-api-key'
target/debug/dimcode --home /path/to/isolated-home --model-config model.json
target/debug/dimcode --home /path/to/isolated-home --model-config model.json \
  --prompt 'Explain this project'
printf '%s\n' 'Explain this project' | target/debug/dimcode \
  --home /path/to/isolated-home --model-config model.json
```

The configuration references credentials by environment-variable name. The shared daemon inherits the environment when it starts; changing the client's environment does not update an existing daemon. Use a separate home for a different explicit model configuration. The client matches the server's explicit product/configured/mock profile and the SHA-256 of the exact configured file bytes; it rejects a mock daemon or a different configuration without deleting the endpoint or stopping that daemon. A configured real model uses production workspace tools, not mock tools.

`--cwd PATH` selects the workspace. `--provider ID --model ID` selects a different configured model for a new session and sends the saved effort/tier preferences for that exact pair, in both the TUI and line client. The server validates the selection; unsupported saved preferences are reported and retained.

## MCP servers

`mcp list` prints tab-separated rows: quoted and escaped ID, source, transport, enablement and
authorization (`sign-in required`, `signed in`, `sign-in expired`, or `—` when absent).
Missing scopes appear below the entry. With no entry in the catalog it says so in one line
instead of printing the column legend, which describes rows.
It does not print commands, environment values, headers or credentials. `mcp enable ID` and
`mcp disable ID`, or `/mcp enable ID` and `/mcp disable ID` in either client, change the entry
that `mcp list` shows for the workspace.

```bash
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp list
# Review the entry in .mcp.json and the code or endpoint it runs, then:
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp enable docs
```

```text
"docs"  project  stdio  disabled  -
                  |
             mcp enable docs
                  v
"docs"  project  stdio  enabled   -
```

A configured MCP server runs when it is enabled; there is no separate approval
([ADR 003](../../docs/adr/003-approval-scope.md)). Entries from a project's `.mcp.json` start
disabled, and the project file cannot enable them. `mcp enable` records the choice for this home,
workspace and ID only; it never edits `.mcp.json`, and it survives daemon restarts. An enabled
project entry runs later edits to its command without asking again and can start discovery in
active sessions. Disabling stops it in every session before the command returns, including a
request that is using it, and works even when `.mcp.json` cannot be read or no longer lists the
entry. Editing the entry out of `.mcp.json` does not withdraw the choice: if the same ID returns, it
is enabled again, so run `mcp disable ID` to withdraw it. `mcp list` prints configuration errors on
stderr. A project entry still shadows a same-name user entry. Pass the ID's actual
value, not its displayed escape spelling, and use the same `--home` and model profile as the daemon
serving your workspace. See the
[MCP configuration contract](../dim-app-server/README.md#mcp-configuration-management).

For an enabled HTTP/SSE entry, run `mcp login ID` or idle `/mcp login ID`. The CLI shows the
resource and authorization server host before opening the browser. It captures one callback
on both loopback families when the configured port is free. While listening, you can also paste
the full URL with hidden input. Unrelated paths and incomplete connections do not consume the
listener. Busy ports, SSH and `--no-browser`
use hidden pasted input. `--client-secret` prompts with echo disabled; it never accepts the
secret as an argument. The daemon owns validation, token exchange and credential storage.

```text
> /mcp
"docs"  project  http  enabled  sign-in required
  Missing scopes: files:read
> /mcp login --no-browser --client-secret docs
Client secret (hidden):
Resource: https://resource.example/mcp
Authorization server: issuer.example
Callback URL:
Signed in to "docs".
> /mcp logout docs
Signed out of "docs". Revoked: true.
```

Browser capture uses this prompt, so remote terminals can paste without restarting:

```text
Waiting for the browser callback. You can also paste the full callback URL now; input is hidden.
Callback URL: _
```

Use `mcp login ID --no-browser --client-secret` for the standalone command; flags can also precede ID. Slash-command
flags precede the ID; its remaining text is the literal ID, including spaces. `/mcp` also
shows the attached Session's server states and a sign-in hint for `auth_required`.
Invalid callbacks can be pasted again. Ctrl+C, EOF or expiry stops waiting and prints the
restart command. Expiry exits 1, cancellation exits 130, and a submitted operation that succeeds
exits 0 even if waiting was interrupted. Once an RPC has been submitted, its reply is drained before another
interactive command can run. A submitted exchange may complete successfully during that wait.
Noninteractive input never starts sign-in; slash text in a headless prompt remains model input.
`mcp logout ID` reports the daemon's `revoked` result. Credentials can be signed out before
removing their entry through the existing configuration API; the CLI has no entry-removal command.

See the [MCP OAuth contract](../../docs/design/appserver/mcp-oauth.md#13-requirements-raised-to-mechanism-owners).

## Permission presets and approvals

The App Server decides each tool call; the CLI selects settings and never decides a call itself.
`/permissions` opens three independent controls, also available under `/settings`, and the
Command rules area:

```text
Permissions
  File access         Read only / Workspace read/write / Unrestricted
  Tool approval       Ask / Bypass
  Process isolation   Off / Sandboxed · No network / Sandboxed · Network allowed
  Command rules       List, add and remove the user's command rules
```

The Command rules row opens the management panel of `Area::Permissions`: one entry per rule,
whose id and label are the rule's value (`DECISION MATCH COMMAND`), with `Ctrl+R` reload, `Ctrl+N`
add and `Ctrl+X` remove. A rule has a decision, not an on/off state. Esc returns to the Permissions
panel.

Choosing File access writes only `fileAccess`; choosing Tool approval writes only `toolApproval`.
The UI preserves the branch's legacy preset and tool lists, and saves the chosen field alone in
`config.json` for new sessions, keeping other configuration keys. Ask asks about all tools except
explicitly allowed tools and commands; Bypass skips only tool questions, not file restrictions,
deny rules or process isolation. The picker identifies legacy risk-based behavior until an
explicit tool-approval choice is made. Existing `permissionPreset` configuration and `--permission`
remain compatibility seeds, with this behavior when the independent fields are omitted:

| Preset | Reads | File writes | Commands, network, unclassified tools |
| --- | --- | --- | --- |
| `read-only` | run | denied | denied |
| `workspace-write` (product default) | run | run inside the workspace; ask outside | ask |
| `full-access` | run | run | run |

Every outside file write asks for that call, even when its tool is explicitly
allowed; no path grant is remembered. The server resolves symlinks and new-file
parents before checking the [workspace boundary](../../docs/architecture/rust-agent/plugins/Permission.md#workspace-write-targets).

`--permission read-only|workspace-write|full-access` works with `dimcode`, `tui`, `exec` and
`exec resume`. A new Session receives it in `session.create`; a resumed Session's `main` branch
is changed with `session.permission.get/update` before attach, keeping its tool lists and
retrying a version conflict. Without the flag a resumed Session keeps its preset. The header
shows the preset; `exec` prints it to stderr only when the flag is given. A live change on
`main`, from any client, prints `[permission] main branch preset: ...`.

At the line-mode idle prompt, `/permissions` (aliases `/approvals` and `/approval`) shows the
independent controls, the legacy preset seed and the user's command rules. It accepts
`file-access read-only|workspace-read-write|unrestricted` and `tool-approval ask|bypass` for the
active main branch. `/approvals PRESET` still switches the legacy seed; `full-access` asks `[y/N]` and only `y`/`yes`
confirms. `/approvals allow|deny exact|prefix COMMAND` adds a rule and `/approvals remove N`
removes rule N of the list printed last; both print the new numbered list. A preset or rule list
that changed since it was shown is reported and nothing is written. Child branches and native members created later from the changed branch inherit its current policy;
existing branches and members retain their own policies, including when a member is reused. Rules are user scope and apply in every Session: a deny rule under
every preset, an allow rule only where the preset would ask.

Member tool approvals are routed through the branch that delegated the work. When the leader's
allow decision needs approval, the prompt shows the original member, Session/branch, tool,
arguments and reason from the server. It offers only Allow once and Reject, and cannot allow
until details load. Details can be retried; rejection remains available during loading. The
member conversation panel stays read-only. See the [member approval contract](../../docs/design/subagent-tool-approval.md).

For a simple `exec`/`execute` command (no `;` `&` `|` `` ` `` `(` `)` `<` `>` `$` or line
break), the approval prompt also offers `a`:

```text
[approval] exec: Tool `exec` requires approval for starting processes under the workspace-write preset
  command: cargo test -p dim-cli
Allow once, always, or deny? [y/a/N] a
Always allow `cargo test -p dim-cli` (exact). Press Enter, or type its leading words for a prefix rule: cargo test
[rule saved] allow prefix `cargo test`
```

The CLI writes the rule through `permission.rules`/`permission.set_rules`, retrying a revision
conflict, and then answers allow. If the write fails it prints `[rule not saved]` and asks
`Allow once? [y/N]` for the same request. See
[permission controls](../../docs/design/cli-commands.md#permission-controls).

## Goal

`/goal` reads and writes the goal of the line the client shows. The methods, the blocks and the
rule that reads them are [Goal §A.2–A.3](../../docs/design/plugins/Goal.md#a2-session-methods) and
[Pursuit](../../docs/design/plugins/Pursuit.md)'s; the client forms are
[CLI commands § Goal](../../docs/design/cli-commands.md#goal)'s.

| Form | Call |
| --- | --- |
| `/goal` | none; the goal in force on the shown line, or that it has none |
| `/goal OBJECTIVE` | `session.goal.edit` while the goal is active or paused, `session.goal.set` otherwise |
| `/goal set OBJECTIVE`, `/goal edit OBJECTIVE` | the named method |
| `/goal pause`, `/goal resume`, `/goal clear` | the method of the same name |

Every call sends the shown line's id as `branchId`: the line client always shows `main`, and the
terminal UI sends the line whose transcript it displays. An objective that is one of the form words
is written after `set` or `edit`, so `/goal set pause` sets the objective "pause". A typed objective
edits a goal that is still being worked toward and starts a new one when there is none, when it has
no status, or when it completed or blocked: a finished goal is never edited back into work.

The goal shown is the client's fold of that line's own events: the latest live `goal:objective` and
`goal:status` blocks, with `clear` retiring both, `edit` keeping the judge, a worker's `completed`
waiting for an evaluator judge, and a deleted input folded out. After a write the client reads the
line's events through the write before it prints, so the report shows the state that write produced.
The fold starts over at the line's own opening — its `branch/start`, or the `session/forked` a fork
writes after the copied facts — so a line opened from another has no goal, and a fork's `main` never
shows the copied one.

```text
> /goal Keep the CLI tests green
Goal set on main · active
  Keep the CLI tests green
> /goal pause
Goal paused on main · paused (user)
  Keep the CLI tests green
> /goal
Goal on main · paused (user)
  Keep the CLI tests green
> /goal clear
Goal cleared on main.
```

In both terminal UI modes the status area carries `◎ goal active` or `◎ goal paused` while the
shown line follows a goal, and nothing for a completed, blocked or cleared one. When the daemon
installs no Goal — its `session.methods` lists no `session.goal.set` — `/goal` prints
`Goal is not available on this daemon.` and calls nothing.

## Execution boundary

The sandbox is a setting, and it is off by default. `/permissions > Process isolation` and
`/settings > Process isolation` open the same picker: Off, Sandboxed · No network, or
Sandboxed · Network allowed. Save, Reset and Reload reuse the existing user-scope execution settings
and their revision conflict checks. The stored `sandbox` and `network` switches both start off;
Network applies only while Sandbox is on. The setting
is read whenever a command starts, so it applies to the next command or new persistent shell
without restarting the daemon, and a running process keeps the boundary it started with. Approving
a command and choosing `full-access` do not change the boundary; the model cannot change it at all.
`exec` has no `sandbox` argument, and a call that still sends one is rejected instead of ignored.

With the sandbox off, commands run as your user with your environment, home and network. With it
on, a command can write only inside the session workspace, read only the workspace and the
platform's system directories, and has no network access until you also turn Network on. The
[process isolation design](../../docs/design/host-platform-isolation.md) lists each platform's
directories and the environment variables passed through. The same switch governs the persistent
shell, whose output directory is writable besides the workspace.

On macOS only, the sandbox also lets common developer tools run:

- Each command gets its own empty temporary directory as `TMPDIR`, writable and removed after the
  command ends. `/tmp` and your own `TMPDIR` are not granted.
- The `/usr/bin` developer tools (`git`, `python3`, `clang`, `make`) can read the selected
  developer directory: `DEVELOPER_DIR` when the daemon's environment sets it, otherwise the one
  `xcode-select -p` prints. The Command Line Tools work; an Xcode.app selection is not supported.
  On such a Mac, run `xcode-select -s /Library/Developer/CommandLineTools`, start dimcode with
  `DEVELOPER_DIR=/Library/Developer/CommandLineTools`, or leave the sandbox off.
- Git can read its global configuration: `~/.gitconfig`, and `config`, `ignore` and `attributes`
  in `$XDG_CONFIG_HOME/git` (`~/.config/git` when unset). Each file is granted on its own, so
  `~/.git-credentials`, the `credentials` file beside them, `~/.ssh` and the rest of your home stay
  unreadable. Files pulled in with `include.path` or `includeIf` are not granted, and git then
  fails with `Operation not permitted`.

A denied operation fails inside the command as an ordinary error: `Operation not permitted` on
macOS; on Linux, `No such file or directory` for an ungranted path and `Read-only file system`
for a read-only one. When a sandboxed command fails with a typical denial, its result tells the
model that the sandbox may be the cause and that you change the switches in
`/settings > Process isolation`.

There is no policy file, and the `host-isolation.json` that earlier releases read is ignored: a
daemon that finds one names it on stderr at startup and asks you to delete it. The switches are
stored as the user-scope
`execution` settings source and written by the panel with revision conflict detection, so a
conflicting edit requires reloading rather than overwriting another client's change.

Two failures stop a command before it starts:

- `isolation_unavailable`: the sandbox is on and the platform has no backend. That is always the
  case on Windows, and on Linux when bubblewrap is not on the daemon's `PATH`. Install bubblewrap
  or turn Sandbox off.
- `isolation_config_invalid`: the daemon could not read the switch. Fix that settings source; a
  command never runs unsandboxed because the switch could not be read.

### Not covered

- The file tools (`read`, `write`, `edit`, `glob` and `grep`) run inside the daemon, not in the
  sandbox. Presets and approvals govern them, and
  [#294](https://github.com/arcships/dim/issues/294) tracks limiting the paths they write.
- Commands you type with `!` or `!!` run as ordinary processes.
- Windows has no sandbox: commands run as ordinary processes.
- MCP servers, plugins and the daemon itself are outside the sandbox.

## Questions

The model's `ask_user` tool asks the user one to four multiple-choice questions and waits
([Questions](../../docs/architecture/rust-agent/plugins/Questions.md)). The CLI asks them one
call at a time, in arrival order, from every branch of the attached Session, and sends the answer
with `session.question.respond`:

```text
[question] Database
1. Which database should the service use?
   1. PostgreSQL  managed, relational
   2. SQLite      single file
   0. Other (type your answer)
Choose 1-2, 0 for other, empty to decline: 1
2. Which features ship first?
   1. Auth
   2. Billing
   3. Search
   0. Other (type your answer)
Choose numbers, comma separated, 0 for other: 1,3,0
Other: audit log
[question answered]
```

A question of another branch shows `(branch NAME)` after its header. `0` is offered only where
the question accepts the user's own text, and several numbers only where it accepts several
options. An empty line on the first question declines the whole call; an empty or invalid line
elsewhere asks again. Approvals, model output and shell results wait while a question is open,
as they do during an approval prompt.

The prompt closes with `[question answered elsewhere]` or `[question cancelled]` when the call's
result arrives first, for example because another client answered or the turn was stopped. A
rejected answer prints `[question rejected]` and asks again. When the call is not waiting yet, or
the request failed, Enter sends the same answer again. `Ctrl+C` or EOF at a question stops that
question's branch, and the turn while it runs, as `Ctrl+C` does for a turn. Without terminal
input, as in `exec`, the CLI prints `Interactive answer required; stopping. Rerun in a terminal
to answer it`, stops the question's branch and the running turn, and exits 3.

### Questions in the terminal UI

Both terminal UI modes use the same Questions client types and response contract.
The existing prompt takes the composer's place; its draft stays intact. Questions
from every branch are recovered through the existing fixed-watermark branch-history
reader and merged with live events. A child question shows its originating branch.

```text
 [ Database ] [ Features ] [ Confirm ]
 Question on branch reviewer
 Which features ship first?
   [x] Auth
   [ ] Billing
   [x] Search
       Type your own answer: audit log
 Space toggle · Enter next · Tab switch · / commands · Esc decline
```

Number keys select choices, Space toggles a multi-select choice, and Tab/Shift+Tab
or Left/Right changes question tabs. Enter on Confirm sends one answer per question;
offered labels and custom text remain separate. The custom row exists only when
`allowCustom` is true. Esc declines the whole call. An existing approval or question
keeps its prompt until handled; leader prompts preempt a read-only member detail.
Member questions never open a response control in the leader UI.

A pending question does not block commands. While its free-text answer is not being
edited, `/`, `Ctrl+O` and `Ctrl+P` open the command palette over it, drawn as the
palette is drawn anywhere else; the question stays pending with its tabs, its
selections and its typed answers. The palette opens command entry, which keeps the
keys through a command's argument, shown on the composer line, until `Enter` runs the
command, `Esc`, or a draft that stops being a command. `Enter` runs the chosen command
as it does during a reply, and `Esc` in the palette or while an argument is typed
closes command entry and gives the keys back to the question, without declining it or
interrupting a run. A command that opens its own panel gives the question back, with
its answers, when that panel closes — unless the Session stopped asking it meanwhile,
for example because another client answered or the turn was interrupted. `/` is the
palette before the free-text row is edited, so an answer may itself start with a path:
Enter, Space, the row's number or any other character enters the row, and `/` typed
inside it is text.

Submission retains the prompt and disables duplicate submissions until its reply.
Success closes this client's prompt; a committed tool result also closes it,
including when it arrives before the reply. Invalid, not-pending and failed replies
retain the answers and show a diagnostic; Enter on Confirm explicitly retries.
Esc can retry a declined response.
Branch/request/call identity and the original Session address travel with the
response, so a late reply cannot change a later question reusing the call ID.
Control characters in displayed headers, labels and diagnostics are sanitized;
responses retain the original labels. Narrow screens use the existing prompt window.

Ctrl+C follows the terminal UI's existing interrupt/exit rules. When it interrupts
an open question, it stops that question's branch and the leader turn if running,
before any separate shell run. It does not introduce a question timeout or withdraw
an already accepted answer. The line-mode and `exec` policies above remain unchanged.

## Scrollback and session recovery

This section describes the `--no-tui` line client; the terminal UI's
equivalents follow in [Terminal UI commands and input](#terminal-ui-commands-and-input).

```text
dimcode · model: openai/your-model-id
workspace: /path/to/project
session: 00000000-0000-0000-0000-000000000000
permission: workspace-write

> Explain this project
Assistant:
The project contains ...
[tool] read
[done] read
[approval] exec: Permission required
  description: Inspect build output
Allow once? [y/N]

> _
```

Only `y` or `yes` permits a pending tool operation once (`a` saves a rule first, as above); empty input or any other answer denies it. Approval and account prompts retain their existing line input and echo behavior. Tool status records and approval reasons are displayed without dumping arbitrary tool arguments or tool result bodies. Model and external text are filtered at the terminal boundary to remove control sequences.

The terminal keeps the scrollback. The draft editor uses the existing terminal
input owner and restores terminal modes and bracketed-paste settings when it
yields. It does not create an alternate screen or a client-side input queue.
Ordinary text typed during execution is discarded with a message to wait for
the prompt; pending typeahead is cleared before an approval prompt so it cannot
accidentally approve an operation.

Interactive `!command` runs in the Session's working directory through `shell.run` and records the command and bounded output for the next model request, without starting a turn. `!!command` keeps its output private to this caller. Both also work during model execution. The prefix must be the first character; leading whitespace
keeps the line ordinary model input. An approval prompt owns its answer line. Commands use the daemon's shell/environment and null stdin. Progress streams under one header per run without adding newlines to partial chunks; the final bounded output is displayed again, including truncation and recording failures. Commands are never retried after an RPC or transport error. Piped or `--prompt` text keeps its literal meaning.

While a turn is running, `Ctrl+C` requests stop and waits for the server's resource and output delivery barrier. A second interrupt does not skip settlement. Once the prompt returns, Ctrl+C clears a nonempty draft or exits an empty one; EOF exits. The client detaches; it does not delete the session or shut down a shared daemon.

During a shell run, the first `Ctrl+C` stops that run; a second stops the model turn. At an approval
prompt, Ctrl+C stops the turn. If only command settlement remains, another interrupt prints its
status. An interrupted RPC may leave its command running and recorded: the caller retains its
runId and Ctrl+C can still stop it. Both replies settle before the prompt returns. EOF is an explicit CLI stop action: it stops the caller's run and turn before detaching. Disconnecting does not stop Session work: a recorded shell result can still appear in history. A second command from this caller is refused until its pending replies settle. [P1 ShellInput](../../docs/design/plugins/ShellInput.md) defines process and background-child lifetime.

```text
[shell interrupted] Run <runId> may still be running. Record-mode output may still be recorded. Ctrl+C stops it.
^C
[shell stopping] Waiting for command settlement
^C
[shell stopping] Still waiting for command settlement
```

Every session handle is printed; noninteractive handles go to stderr. Restore it explicitly:

```bash
target/debug/dimcode --home /path/to/isolated-home --model-config model.json \
  --resume SESSION_ID
target/debug/dimcode --home /path/to/isolated-home --model-config model.json \
  --resume SESSION_ID --prompt 'Continue'
```

Interactive recovery displays main-branch history. Noninteractive recovery emits only the current output. History uses a fixed server watermark and its real `nextAfter` positions, including filtered records. Live events and progress snapshots are reconciled with already printed durable text. The client waits for an existing turn before accepting a new prompt. It never automatically retries a submission after a transport failure because the server may already have accepted it.

## Terminal UI modes

The interactive terminal UI has two modes over the same session. Scrollback (the default) commits the transcript into the terminal's native scrollback and repaints only the composer region at the bottom. Its ground rule: a committed row is never taken back. Nothing above the live region is erased, shortened or redrawn to fewer rows, because the composer sits at the bottom of the screen and the terminal would pull it up by every row removed; corrections append or rewrite a row in place, never shrink. Inside the live region the composer (or the question or approval in its place) is the anchor and never moves up while the region has room below: fewer rows above it (the live tail, the queue strip) give back the region's top rows, blank, as a gap above it, and fewer rows below it (a menu or picker closing, a question changing tab or answered) give back its bottom rows, blank, under the status row. Scrolling the screen cannot be undone, so the region grows into those blank rows first, below or back up into the gap, and scrolls only for rows that fit nowhere; a menu or question opened and closed again and again scrolls history once, the first time. The next commit fills the gap, so no blank rows are left in history. No row is ever blanked and then drawn again in the same frame: rows are written over in place, a row wider than the screen is written once and wrapped by the terminal over the rows below it, and a row exactly as wide as the screen gets no trailing erase (it would blank its last cell). Fullscreen takes the alternate screen, keeps the transcript itself and scrolls it in a viewport above a dock that never moves. A transcript shorter than the viewport sits on the dock and grows upwards:

```text
 answer row                                                     ▁   scrollbar: last column,
 answer row (streaming tail scrolls with the transcript)        █   thumb in eighths of a row
                                                                █
 ╭─────────────────────────────── provider / model · effort ─╮      dock: queue, composer
 │                                                            │      and panels stay pinned
 │                                                            │      to the bottom; the box
 │                                                            │      borders carry the model,
 ╰─ · working…  3s  esc interrupt ──── ~/project (branch) ─╯      the status and the place
```

| Fullscreen input | Effect |
| --- | --- |
| Mouse wheel, `PageUp` / `PageDown` | Scroll the transcript; new output does not move a scrolled view. A wheel notch scrolls three rows; a trackpad, once its dense reports are seen, one row per report; a fast streak speeds up to three (notch) or six (trackpad) times that |
| Sending a message | Follow the bottom again |
| Mouse drag | Select transcript text; release copies it (OSC 52, plus `pbcopy` locally on macOS; inside tmux OSC 52 goes through passthrough, which needs `set -g allow-passthrough on`) |
| Click a link | Opens it in the system browser (OSC 8 links, or a bare `http(s)://` URL); over SSH the status row shows the URL instead |
| Click a tool group's header or a thought's title | Opens (`▾`) or closes (`▸`) that group, or shows a settled thought whole or as its one-row summary; a running group keeps the form it is given when it settles. A settled block opens in place: the view stops following the bottom so the header stays under the pointer. A press counts as a click only if no drag followed and its row still shows the same text at the release, so output arriving in between cannot redirect it. `/tools` sets every group and thought, settled ones included, and lays the transcript out again from the bottom |
| `Tab` / `Shift+Tab` | Pick older / newer user messages (`Up` / `Down` while one is picked); past the newest the pick ends. `Esc` ends it, `e` puts the message in the composer to edit and send again; `b` opens a line of the shown line cut just before the message, with the message in the composer as its opening input (`Enter` opens it through `session.branch_create` and shows it, `Esc` cancels); `f`, on `main` only, asks, then forks the Session just before the message (`session.fork`) and attaches to the fork with the message in its composer. A `⑂ line` row, written where a line opened on the shown line, is picked too: `Enter` (or a click) shows that line ([timeline UI](../../docs/design/timeline-ui.md)); so is the load-more row at the top, which `Enter` loads |
| `/`, `Ctrl+O`, `Ctrl+P` | Open the command palette centred over the dimmed transcript, with its own query row; a draft set aside by `Ctrl+O`/`Ctrl+P` returns when the command entry it opened ends (`Enter`, `Esc` or an emptied draft). With a question pending whose free-text answer is not being edited, the palette opens over it instead, and the question keeps its answers while the command's argument is typed |
| `Home` / `End` | With an empty composer: scroll to the top / bottom of the transcript |
| `Esc` | Interrupt the run |
| `Ctrl+C` | Clear the draft, then exit on a second press; a busy session asks for confirmation instead of being interrupted |

Edit group headers name the edited paths even while collapsed. A single file replaces the
file count; multiple files keep the unique-file count and total added/removed lines before
the path list, in first-call order. Repeated edits of one path count as one file, while all
successful edits contribute to the line totals. Mixed tool groups carry the same edit summary.
Paths retain the tool input's spelling, including directory components to distinguish namesakes.
On narrow terminals, the path list gives way to a localized remaining-file count; an individual
path may shorten on grapheme boundaries to keep the totals and disclosure visible. If even
those fixed fields exceed the width, the header uses the ordinary one-row truncation.
Opening the group still shows each tool's full path and diff.

```text
✓ Edited src/main.rs (+12 -3) ▸
✓ Edited 2 files (+18 -5) · src/main.rs, src/config.rs ▸
✓ Edited 5 files (+42 -16) · src/main.rs, … 4 more ▸
✓ Ran 1 command, edited src/main.rs (+12 -3) ▸
```

Interrupting a turn immediately shows a transient `Stopping` notice in the status slot. The transcript records the user-interruption note from the confirmed cancellation, after the answer has finished revealing and immediately before `stopped`, once per cancelled turn. Reopening preserves this order for turns interrupted by this version of the TUI. Older cancellations and cancellations from the line client retain their existing `stopped` display.

```text
answer text (including its retained streaming tail)
· User interrupted the run.
· stopped
```

The composer box: no prompt mark or placeholder, the adopted model and, when its configuration carries one, the reasoning effort it sends (`provider / model · effort`) at the right of its top border, and in its bottom border the run status, the status slot (a notice, `esc interrupt`, `permission required`), the main branch's context reading (`1.8K / 256K`) and a non-default approvals mode at the left, the working directory and git branch at the right. A box too narrow for the whole label drops the effort first, then the label. Fullscreen has no status row under it; scrollback keeps its status row and `›` prompt.

The Git branch label follows branch switches in both modes, including switches made by tools or another terminal. At startup the client resolves the workspace's own Git `HEAD` path (including linked worktrees); the existing UI timer reads that file at most once every 10 seconds, without spawning more Git processes, and requests a repaint only when the name changes. Detached HEAD displays `HEAD`; a transient read failure keeps the last known name until the next check.

If the workspace is not a Git repository at startup, the label stays `Not available`; initializing a repository later requires restarting the TUI to discover it. The client does not repeatedly spawn Git to look for a new repository.

```text
git switch feature/fix  ->  workspace HEAD changes  ->  next check (within about 10 s)
                                                      |
composer:  ... (main)  --------------------------------+->  ... (feature/fix)
```

The context reading is the estimate the next request sends against its window, and a compaction threshold the server returned with it. It is read when a session is entered, after a model switch, after each model request on `main` ends, when a run on `main` ends, and when a compaction result arrives on `main`, so the reading follows a long turn and a compaction outside a run does not leave it stale. At or above 90% of that threshold — the interface's `CONTEXT_WARNING_PERCENT`, applied to the returned `thresholdTokens` — the reading is drawn as a warning and says `near the compaction threshold`. The client never computes the threshold itself; the formula belongs to compaction.

A context compaction has a row of its own in the transcript. `context/compaction-start` opens it in the running state, and the `context/compaction` whose `start` names it settles that row; a result without a start, a manual compaction that failed before a strategy ran, gets its own row. The row shows what the record holds:

```text
 › try the RwLock for the cache lock
 … reply …
 · Compacting context…
 ✓ Context compacted ▾
 ▎ <dim-summary>
 ▎ The cache lock moved into a RwLock; the parser race is next.
 ▎ …
 ▎ Tool effects
 ▎ <dim-digest>
 ▎ …
 ▎ wrote src/cache.rs
 ▎ ran exit 0: cargo test -p cache
 ▎ Kept verbatim
 ▎ Try the RwLock before touching the parser.
 › carry on
 ✗ Context compaction failed · the summarizer returned nothing
 ◦ Context compaction skipped · nothing to cut
 ◦ Context compaction cancelled · the capability was reloading
 ✓ Context compacted · insufficient space: summary 3.2K, pinned inputs 1.1K ▸
```

`✓` is a completed compaction, `✗` a failed one, `◦` one that did not apply, and the skipped, failed and cancelled rows carry the reason or error of their record. The text a completed compaction wrote in place of the cut history is the row's detail, opened and closed the way a tool group's body is: a click on the row, or `/tools` with the other details (`▸` closed, `▾` open). The detail is split where the record says it is: with `replacement_parts`, the strategy's messages are the summary, the digest the runtime recorded from the effects tools declared follows under `Tool effects`, and the inputs the compaction pinned follow under `Kept verbatim`; a record without the field shows the whole replacement as the summary, with no digest and nothing pinned. A record whose parts cannot meet the bound carries `insufficientSpace`, and the completed row's header names its parts largest first, with the token counts the gauge's formatter gives them — `insufficient space: summary 3.2K, pinned inputs 1.1K`. The part names are localized labels and the mark is read from the record; the client never decides it. The transcript keeps every earlier row and the compaction adds no boundary of its own; a resume rebuilds the rows from history as they were drawn live. [Compaction requirements](../../docs/design/compaction-quality.md#4-what-users-see-and-control) say what a compaction row must show.

`/models` uses Ctrl+X to clear the highlighted model's saved reasoning/tier preferences with the displayed record version; Enter adopts it separately. For noninteractive recovery, run `dimcode models PROVIDER --clear-preferences MODEL`. A concurrent change fails and requires a reread/retry, and the reread lands before that notice, so a retry uses the record's current version; preferences are never silently discarded.

Pickers (`/resume`, `/models`, `/connect`, `/login`, `/approvals`, `/theme`, `/language`, `/settings`) open as centred boxes too; the mouse wheel moves their selection. Approval prompts, question prompts and the exit confirmation take the composer's place until they are answered, in both modes: there is nothing to type into meanwhile. A question is the exception: while its free-text answer is not being edited, `/`, `Ctrl+O` and `Ctrl+P` open the palette over it (see [Questions in the terminal UI](#questions-in-the-terminal-ui)). The live tail above (the tool group waiting on the answer, a running thought) stays where it is. Every panel is framed the same way: the title in the top border, the keys in the bottom one (`╰── Enter select · Esc close ─╯`, keys in the keybinding colour), and a filterable list has a `> query` row between blank rows instead of a count; the query is a line editor like the composer's (cursor keys, Home/End, Delete, pastes).

```text
 ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓    picked message: the box
 ┃ ┃ Explain this project                                   ┃    takes the blank rows
 ┗━e edit · b branch · f fork━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛    around it, nothing moves
 The project …        ╭─ Command Palette ─────────────────╮
                      │                                    │    palette: own query row,
                      │ > th                               │    transcript dimmed behind,
                      │                                    │    the selected row on an
                      │▓ view  /theme  switch theme ▓▓▓▓▓▓▓│    accent bar
                      │   app  /exit   exit   Ctrl+C Ctrl+C│
                      ╰────────────────────────────────────╯
```

Both scrollback and fullscreen palettes list slash commands, grouped by category and sorted alphabetically by canonical English category and command name. Each row shows its category, `/command`, description and any command shortcut. Commands requiring a session are faint and marked `(unavailable)` until one exists. Keyboard-only actions are omitted from both menus; `/keys` lists `Enter` queue and `Ctrl+S` steer while a run is in progress (`Enter` on an empty draft steers too), `Esc` interrupt, and the fullscreen `Home` / `End` scroll and `Tab` message picker bindings.

`/permissions` is the single visible permission entry; `/approvals` and `/approval` are compatibility aliases for the same unified flow. Selecting an incomplete `exit` or `suspend` match only fills the command name; its exact name or alias is required before dispatch. Thus `/` + Enter completes `/exit` without quitting, and another Enter executes the completed command.

Typing ranks the matches: the exact name, a name prefix, an alias prefix, a prefix of a name part (`reload-skills` for `sk`), a name containing the query, a description word starting with it, then three or more letters in order through the name (`stg` for `settings`); a match inside a description word does not count. Equally ranked matches use the category/name order. See the [command menu design](../../docs/design/cli-commands.md#command-menu).

`/settings` gathers the interface settings: theme and language (each opens its picker and comes back), the startup form (`tuiMode`), tool details (`tuiToolDetailsExpanded`: expanded by default in both modes; collapsed shows each tool group as its header, folds thoughts and closes a compaction row's summary, digest and kept input, and `/tools` flips it too), the fullscreen mouse (`tuiMouseMode`: off hands selection and the wheel to the terminal), notifications (`tuiNotifyOnComplete`, on by default), ACP Sessions in `/resume` (`showAcpSessions`, off by default), tool approvals and execution settings. Model selection stays in `/model` (`/models`). Switches flip with `Enter`, `←`/`→` or `Space` and are saved to `config.json` at once.

Notifications reach a user who switched windows when the run finishes or waits on a permission or a question (each request once): iTerm2 gets `OSC 9`, kitty `OSC 99`, Ghostty, WezTerm and foot `OSC 777`; tmux and any other terminal get a bell, which tmux forwards and terminals turn into a Dock bounce, a taskbar flash or a tab badge.

The terminal UI also tells the terminal what the run is doing now, with the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status) (`OSC 7501`), so tab indicators, session pickers and agent inboxes can show it without reading the screen. Scrollback and fullscreen report the same way. Every report goes to the terminal's root record with `app=dim` and ends with `ESC \`:

| What dim is doing | Report |
| --- | --- |
| At the prompt, no run | `state=idle` |
| A run in progress | `state=working` |
| A permission request is pending | `state=blocked:kind=permission`, with `msg` naming the tool (`Permission needed: exec`) |
| A question is pending | `state=blocked:kind=question` |
| A run this interface watched has finished | `state=done` |
| A run this interface watched has failed, or the App Server connection was lost | `state=error` |
| A run was interrupted | `state=idle` |

```text
 in dim                          the terminal's record
 start, at the prompt       ->   idle            (the first report is always written)
 send a message             ->   working
 exec asks for permission   ->   blocked  permission  "Permission needed: exec"
 allow                      ->   working
 the turn completes         ->   done
 /exit, /suspend, a signal  ->   (cleared)
```

A pending request outranks the run it holds up. `done` and `error` come only from a turn that ended while this interface was attached: resuming or switching to a Session whose last run ended earlier reports `idle`. A report is written only when the status changes, between frames. `msg` is one line of the interface's own text, with control characters removed, at most 2048 bytes and base64-encoded; it never carries prompts or tool arguments. Once a report was written, leaving clears the record with `state=clear`, also from the restore after a panic or a fatal signal and on `/suspend` (the next status is reported again when the job continues); nothing is cleared when nothing was reported. The protocol has no clear for the root record alone: `state=clear` without an id removes every record on the terminal, so records other programs left under their own ids, before dim started or while it was suspended, go too. None of dim's own processes report meanwhile: the App Server and the tools it runs have no terminal.

Reporting is on by default and independent of `tuiNotifyOnComplete`, because a status report does not interrupt anyone. `DIM_TERMINAL_STATUS=off` turns it off, for example in the profile of a terminal that prints unknown escape sequences instead of ignoring them. Only the terminal UI reports: `dimcode exec`, `-p`, redirected stdin and `--no-tui` never write a report, so `--json` output and redirected output stay clean. Inside tmux the reports stop at tmux, which keeps an `OSC` it does not know to itself; they are not wrapped for passthrough as `OSC 52` is, because the outer terminal would keep one record for all of the tmux client's panes, and a pane killed mid-run would leave its `working` behind.

Terminals that draw a progress bar for `OSC 9;4` (ConEmu's sequence, which Windows Terminal also shows on the taskbar button) get one from the same status, in both modes:

| Status | Bar |
| --- | --- |
| `working` | indeterminate, `OSC 9;4;3` |
| `blocked` (a permission or a question) | paused at full width in the terminal's warning colour, `OSC 9;4;4;100` |
| `idle`, `done`, `error` | none, `OSC 9;4;0` |

```text
 the terminal's record        the bar
 idle                    ->   (none)
 working                 ->   [~~~~~~~~~~~~]  animated by the terminal
 blocked  permission     ->   [############]  paused, in the warning colour
 working                 ->   [~~~~~~~~~~~~]
 done                    ->   (removed)
 /exit, /suspend, signal ->   (removed)
```

A bar means a run is under way or held up, so a finished or failed run removes it; the result stays in the record. `OSC 9;4` shares its number with iTerm2's notification, and a terminal that knows only the notification shows every update as one, so the bar is drawn only where it is known to work: Windows Terminal (`WT_SESSION`), ConEmu (`ConEmuANSI=ON`), Ghostty (`TERM_PROGRAM=ghostty` or `TERM=xterm-ghostty`), WezTerm, and iTerm2 from 3.6.6. Inside tmux, GNU screen or herdr there is no bar: tmux and screen keep the sequence, the outer terminal's variables leak into the pane, and passed through, the outer terminal would show one bar for all panes and keep a killed pane's. `DIM_TERMINAL_PROGRESS=off` turns the bar off and `on` draws it in any terminal, for one this list does not recognise; it is independent of `DIM_TERMINAL_STATUS`. Ghostty drops a bar it has not seen for about 15 seconds, so a bar that shows is drawn again every second. Once a bar was drawn, leaving removes it, also from the restore after a panic or a fatal signal and on `/suspend` (it is drawn again when the job continues). As with the reports, only the terminal UI draws it.

`/keys` writes the interface's keys into the transcript; in scrollback it adds how the terminal that owns the history scrolls, searches and copies (tmux copy mode, iTerm2 and Apple Terminal `Cmd` keys, `Shift+PageUp` in Ghostty, WezTerm, kitty and Alacritty, Windows Terminal).

`/language` (`/lang`, `/locale`) picks the interface language: the system default (`LC_ALL`, `LC_MESSAGES`, `LANG`) or one of English, 简体中文, 日本語, 한국어, 繁體中文, Bahasa Indonesia, Deutsch, Français, Español, Português (Brasil) and Português (Portugal). Moving previews, `Enter` saves it as `locale` in `config.json`, `Esc` restores. Every language is translated; a string missing from a table reads as English. Rows already in the transcript keep their language.

Without a saved `themeId`, a dark terminal with truecolour or 256-colour support defaults to `Dim Dark`. Light backgrounds and 16-colour terminals use `System`. Apple Terminal (`TERM_PROGRAM=Apple_Terminal` or `TERM=nsterm…`) also uses `System` when its background is unknown; it only defaults to `Dim Dark` when an OSC 11 background report, `COLORFGBG` or an explicit `DIM_TERMINAL_APPEARANCE=dark` (also `DIMCODE_TERMINAL_APPEARANCE`) identifies a dark background. Detection keeps that precedence: explicit appearance override, OSC 11, then `COLORFGBG`. A saved theme choice is preserved; detection does not write it to configuration.

```text
 no saved theme
   dark + truecolour/256 colours  -> Dim Dark
   light / 16 colours            -> System
   Apple Terminal, unknown bg    -> System
 saved theme                     -> saved choice (existing compatibility checks)
```

In `System` on a dark background every colour is the terminal's own default foreground, a slot of its 16-colour palette (`ansi:N`) or the faint attribute, so it follows the terminal's colour scheme. Yellow is the accent (titles, the selected row, inline code), green the user's messages and successes, blue first-level and cyan second-level headings, red errors; muted text is faint, table rules bright black, and code is highlighted in the terminal's base colours (keywords and types blue, calls cyan, numbers yellow, strings green).

On a light background (Terminal.app's default profile among them) the coloured roles do not come from the 16-colour slots: a light profile's ochre yellow and sea-green cyan were not chosen for reading on white. As in the TS build, each hue takes a fixed lightness that carries it on paper (amber, olive green, blue, teal, red) and is held to the contrast floors against the background the terminal reported (an off-white when it did not answer); muted and faint take fixed greys too, since the faint attribute is a pale grey on white, and the selected row carries white text; text and border stay the terminal's own, which it already pairs with its background. 16-colour terminals keep the slots. `Dim Light` takes the same ramp.

Answers are laid out at the margin with no mark, `-` list items that wrap back to the margin, fenced code indented four columns without its fences, and tables with round corners and a divider under every row. Thoughts keep Dim's own form. `Dim Dark` and `Dim Light` take the same structure with Dim's green as the accent. A theme role is a colour spec: `#rrggbb`, `ansi:N`, `default` or `dim`.

`e` cannot rewrite history: the App Server has no rewind or fork, so the edited text is sent as a new message.

Streaming is paced for the screen: the session worker publishes a snapshot one frame after the first change however fast events arrive, and snapshot rows are shared with the worker rather than copied. Text is revealed at a steady rate that follows the stream: an average of what arrived over the last ~0.6 s, keeping ~0.3 s in reserve to bridge the pauses between bursts, so a bursty stream reads as an even flow at the model's real speed; an answer that ended drains within ~0.4 s. A frame waits while the last one is still being written, so a slow terminal (ssh, tmux) gets fewer, current frames instead of a queue of stale ones. An open code fence commits its finished lines as they arrive, laid out as the closed block will be, so a long block never piles up above the composer (a mermaid fence is drawn once its source is whole, with a two-row placeholder meanwhile; tabs expand to four-column stops); in scrollback, rows committed from the top of the live region stay where they are drawn, so streaming neither erases the region nor moves the composer, and a changed row is rewritten in place before the rest of it is erased.

The transcript is a virtual list. The CLI keeps each committed block once, as its source (Markdown text, tool records), up to 5,000 blocks; fullscreen keeps only every block's height for the current width and renders the rows of the blocks within about a screen of the viewport, dropping them as it moves away. Heights start as estimates (Markdown by its wrapped source lines) and become exact when the viewport first reaches a block, so a settled resize or theme change lays out only the blocks around the view. A scrolled view is held by its top block, so it stays put while heights above it settle and across a resize. The scrollback interface lays out the newest 10,000 rows of the same history when it rewrites the terminal's scrollback.

`/tui` switches modes inside a running session, including during a run. Entering fullscreen erases the scrollback composer and lays the history out on the alternate screen; leaving rewrites the history into the terminal's scrollback. Exiting fullscreen returns to the shell's screen and prints the resume panel there.

```bash
dimcode --fullscreen            # this run only
dimcode --scrollback default    # save the default mode and exit
```

The default is saved as `tuiMode` in `<home>/config.json`.

## Background task inventory

`/tasks` (alias `/background`) shows commands and delegated member work in the
attached Session, including when main has no messages. The worker reads each
discovered branch's local history through a fixed watermark, then applies its
buffered live events. State notices discover restored children and nested
branches. Reopening keeps this projection; it retries only failed pages, from
their last validated cursor. Loading or failed history disables stop-all while
known individual targets remain usable. A failure reading main history is a visible
connection failure: submission is disabled and the unsent draft stays in the current UI.
Recovery requires exiting and resuming the Session.
Resuming waits for main's complete initial history before drawing the latest
100 messages and its boundary. Fullscreen tops them with a load-more row: a
click, or `Tab` up to it and `Enter`, draws the previous 100 above and keeps the
oldest drawn message at the top of the view. Scrollback reaches earlier messages
through a `/timeline` jump; a draft typed meanwhile stays editable.

```text
> paged-message-051
...
> paged-message-150
--- resumed paged · 100 messages · earlier history: /timeline ---
[ draft typed while history was loading                         ]
```

Child-branch failures remain local to the task history and release buffered
events. Reopening completes the retained fixed window, then reads a fresh
catch-up window to recover facts received while the branch read was failed.
Live events remain buffered while a history request is pending.
History pages are adopted as a whole; an invalid event leaves the validated
cursor unchanged. Switching away and back to the same Session creates a fresh
projection. An already-issued old page settles before another is sent, while
its rows and queued retries are discarded.

The same collection keeps all unfinished work and the latest 50 terminal facts,
ordered by the terminal event timestamp and then branch/start-event identity.
Only actual command ends, delivery failures and delegation results finish rows.
The status strip counts unfinished work. Enter shows a command's live output or
its recorded final result, or opens a delivered member's read-only conversation.
Finished delivery results remain available even without a member Session.

```text
+-- Background tasks --------------------------------------------+
| > cargo build           command   Running      12s             |
|   Survey the crates     agent     Starting                     |
|   cargo test            command   Failed        4s  exit 101   |
| Enter details · x stop selected · Ctrl+X stop all              |
+----------------------------------------------------------------+
```

Ctrl+X captures the displayed attachment's known unfinished targets once and
sends the existing addressed stops serially. Repeated delegations to one member
produce one interrupt. Replies and errors advance the batch; terminal facts
update rows independently. Closing the panel or switching Sessions does not
retarget an admitted batch. Feedback reports attempted requests, errors and
starting members without a target; each stop error names its command branch/task
or member Session. New work, later Steer/report effects and, when a member profile
supplies them, members' background commands can continue; stop-all does not stop
the leader.

The implementation follows the [task-inventory contract](https://github.com/arcships/dim/blob/main/docs/design/background-task-list.md)
and the [member-conversation contract](../../docs/design/member-conversation.md).

### Workflow runs

When the connection advertises `workflow.status`, the same list shows Workflow
runs of the attached Session: runs its `workflow` tool started on any branch and
management runs that report to it. Run IDs come from `workflow` tool results'
structured fields and `dim-workflow kind="report"` inputs; the worker reads one
known unfinished run at a time, at most once a second, and stops at its terminal
state. Opening `/tasks` asks for the retained run array once; nothing reads the
whole array while the panel is closed. One status read is outstanding at a time.

```text
+-- Background tasks --------------------------------------------+
| > nightly-check       workflow · Running · 133s                |
|   cargo build         command · Running · 18s                  |
|   managed run         workflow · Completed · 120s              |
| Enter details · x stop selected · Ctrl+X stop all · Esc close  |
+----------------------------------------------------------------+

+-- nightly-check - workflow ------------------------------------+
| Run: <run_id> · Source: user · Running                         |
| Stages                                                         |
| 1  analyze  completed  <stage session>                         |
| 2  tests  running  -                                           |
| analyze result                                                 |
| <stored result text>                                           |
| Log                                                            |
| Report: none                                                   |
| x stop workflow · Esc back                                     |
+----------------------------------------------------------------+
```

`x` sends `workflow.stop` for a `running` run and is disabled while that request
is pending or the run is `stopping`; only the owner's returned run changes the
row. Ctrl+X adds known running runs after the command and member stops, because
a Workflow stop waits for its stages. A read sent before a stop reply cannot
replace that reply's run. While discovery is loading or failed, stop-all is
unavailable; a failure keeps the last observed rows and stops automatic reads
until `/tasks` is reopened. Without `workflow.status` the panel shows no Workflow
rows; without `workflow.stop` the runs are read-only. The contract is the
[Workflow task view](../../docs/design/workflow-task-view.md).

Native App Server flows verify restored child/nested branch discovery, local
command histories, addressed stops and continued leader input. Built-in member
executors keep commands in the foreground even when `runInBackground` is
requested. A member interruption reports its cancelled delegation through Steer;
a later Steer message can start another member turn without reopening that
delegation. The conditional wake check applies only to a profile that supplies
member background commands; no such profile is exercised here.

Member details show the delivered Session ID, its own main conversation and the
delegation's actual result as separate content. The leader's draft, model and
permission display stay in place. Typing or pressing Enter in the detail never
submits input; `x` explicitly interrupts an unfinished member's current turn.
Esc returns to the list and detaches only the member observation. A pending
attach/page settles before cleanup or a subsequent selection. Errors keep the
read prefix visible and discard buffered notices; reopening starts a fresh observation.
A rejected member detach produces a local notice and ends that observation
without closing the leader's connection or claiming server cleanup succeeded.
A member close marks
only its detail closed and leaves the leader usable. Viewing does not run the
editable resume path or apply `--permission` to the member.

```text
+-- Inspect member code - read-only -----------------------------+
| Member Session: <delivered Session ID>                         |
| Observing conversation                                        |
| user       Inspect the imports                                |
| assistant  Reading the crate boundaries                       |
| x interrupt member · Esc back                                 |
+----------------------------------------------------------------+
```

## Terminal UI commands and input

The terminal UI reaches the same Session methods as the line client:

| Input | Effect |
| --- | --- |
| `!command` / `!!command` | Runs through `shell.run` on the line whose conversation is shown, in record or private mode; its progress streams under the composer, `Esc` or `Ctrl+C` sends `shell.stop`. A recorded run comes back as the Session's input event and is drawn as a shell block, also after resume; a private run, or one that could not be recorded, is drawn only here |
| `/mcp`, `/mcp list` | MCP servers with their authorization, then the Session's server states; a state line carries its reason below it: a failed server's message, `Version VERSION rejected: REASON` for a refused tool-source version, and one `MCP catalog: ERROR` note for the Session. With no server configured, one line says so instead of the column legend. Without a Session it shows the configuration alone, the report `mcp list` prints |
| `/mcp login [--no-browser] ID` | Starts sign-in, opens the browser unless `--no-browser` or SSH, and catches the callback on a loopback listener when the redirect port is free; `/mcp callback URL` completes it with a pasted callback instead. A client secret needs hidden input, so `--client-secret` points to `dimcode mcp login ID --client-secret` |
| `/mcp logout ID` | Signs out and reports whether the token was revoked |
| `/mcp enable ID` / `/mcp disable ID` | Turns a server on or off with the request `dimcode mcp enable` / `mcp disable` sends and the same result text. Enabling a project entry runs this workspace's `.mcp.json` entry, including later edits, in active and new sessions; disabling stops it in every session before the call returns. The next `/mcp` lists the new state |
| `/plugins` (`/plugin`) | Lists every plugin name from `plugin.list` (built-in, default, installed): the candidate in use, its state, whether it is locked, and for the selected one its candidates and any error, wait or refused content. `Enter` turns an off plugin on (`plugin.enable`) or, after a confirmation, turns it off (`plugin.disable`); a locked one says why it cannot be turned off. `r` reloads it (`plugin.reload`), `Ctrl+R` lists again. The list is read again after each change, and a failed call shows the method's message |
| `/plugins install PATH` | Installs and turns on the plugin directory at `PATH`, relative to the workspace (`plugin.install`), then opens the list with the outcome, including when a restart is required. Git installs, update and remove are `dimcode plugin ...` |
| `/skills` | Lists the Skills of the Session's workspace from `skills.catalog`, grouped under the source each was found in (`user`, `project`, `plugin:<id>`), with its state and, for the selected one, its description and file; the catalog's issues follow the entries. `Enter` or `Space` enables or disables the selected Skill (`skills.set_enabled`) and the list is read again; `Ctrl+R` rescans the roots (`skills.refresh`). A daemon without the methods says it cannot manage skills |
| `/reload-skills` | Runs the rescan of the Session's workspace with no panel and reports the entry and issue counts it found, plus each issue |
| `/hooks` | Lists each hook source of the Session's workspace as an entry from `hooks.catalog`, with its state and detail; `Enter` or `Space` enables or disables the Session's project source (`hooks.set_enabled`), `Ctrl+E` opens the user document in `$EDITOR` and writes it with `hooks.save` and the revision the panel read, `Ctrl+R` reads the area again. A plugin source is read-only and offers no write |
| `/scheduled` (`/schedule`) | Lists the jobs the Scheduler holds from `schedule.list`, one entry per job with its schedule, next window and last outcome as its detail, and a job whose schedule the owner cannot use marked with that error; jobs are the host's, not the Session's or workspace's, and only a new job's default `cwd` comes from the Session. `Enter` or `Space` enables or pauses the job (`schedule.set_enabled`), `Ctrl+E` edits the job's document in `$EDITOR` with the revision the list reported (`schedule.save`), `Ctrl+N` creates a job from the document the adapter supplies, `Ctrl+X` removes the job after `Remove NAME? Enter confirm · Esc cancel` (`schedule.delete`), `Ctrl+T` runs it now (`schedule.run_now`) and reports the state the owner returned, `Tab` asks the inspect kind, which the Scheduler serves as history (`schedule.history`), and shows the job's recent runs in place of the list with `Esc` returning to it, `Ctrl+R` reads the list again. An edit sends only the members the draft changed |
| `/login` | Opens the Dim sign-in panel. It shows the account from `account.status` as a status line; signed out, its entries are browser, enterprise-email or device sign-in through `account.login.*` at the entry's account directory (below), each entering its own flow (`Esc` after a failure returns to the entries); signed in, it shows the account's name and email, its plan, credits, token windows and feature meters from `account.usage` (`Ctrl+R` asks again; a plugin without the method shows why), and its entries open `/models`, open the plan page at the account directory's `/console/subscription` in the browser, or sign out with `account.logout`, after which the panel shows the sign-in entries again. Browser sign-in redirects to `http://localhost:54321/auth/callback` (`--redirect-uri` overrides it), catches the callback on a loopback listener when that port is free and opens the authorization URL (neither over SSH); a callback URL pasted in the panel completes it too; device sign-in shows the code and polls the attempt every second. `Esc` closes the panel and cancels a pending attempt. On success the catalog is refreshed with `provider.refresh` and `/models` opens |
| `/connect` | Reads safe connection snapshots with `provider.management`. `Test + apply` probes the unsaved draft, then updates only changed fields at the form's original version with `provider.update`; a record whose models are its own has them replaced by the new endpoint's listing in that same write when the edit changes `baseUrl`, `credential` or `headers`, so the catalog follows the new endpoint, while a proxy or timeout edit saves alone and a builtin whose models are the bundled catalog's keeps them; unsupported model listing still permits saving and reports that the key was not tested. Other probe failures keep the draft. `Disconnect` atomically clears credentials and disables the record, and a catalog of the record's own goes with it, because the listing taken with that key is that key's view; the next connect lists the endpoint again. Enable/Disable update only that flag. `Add custom provider` probes a Name, Base URL and API Key draft, then calls `provider.create` with the form's retained UUID and discovered defaults. IDs never derive from names. Failures retain edits; rereads update the safe status/version and require confirmation before retrying. `Ctrl+R` rereads without discarding the draft. Fields and filters use the composer's line editor. A row whose entry carries an `account` descriptor opens the account step instead of the form: signed out, its row says `Sign in` and opening it starts the only flow the descriptor's `loginMethods` list, and signed in, it shows `Signed in` and signs out through `<namespace>.logout`; a daemon that does not advertise the method reports that instead; a row whose namespace is `account` opens the Dim account's `/login` panel instead |
| `/resume` | Lists saved Sessions across workspaces, 50 at a time through `session.catalog`; moving near the end loads the next page. Each row is labelled by the Session's creation origin — `[Chat]`, `[ACP ]` or `[Agent]` — and reads its title, else its first message, else its id. Sessions Dim created for its own work are not listed; ACP Sessions are listed only while `/settings` → Show ACP sessions is on, and only on a daemon that advertises `session.catalog.origins`. The filter searches the server after 150 ms, including Sessions not loaded yet. Ctrl+R refreshes; closing, refreshing or replacing a search cancels the pending read when the daemon advertises `session.catalog.cancel`. Older daemons use `session.summaries` and local filtering |
| `/models` | Lists enabled, executable models from `provider.list`/`provider.catalog`; a model whose context window or maximum output no source declares is not offered. Every provider with such a model keeps one muted row after its models, counting them and naming `dimcode models PROVIDER --set-missing-limits`; when the provider has nothing selectable, that row replaces its empty-provider row. A static provider whose catalog holds no models is asked to fill it with `provider.refresh` once per catalog version in a connection when the daemon says its models can be listed, and the picker shows what the catalog then holds, because the Provider owner decides what a catalog holds; a successful refresh records the version it wrote, so a listing that comes back empty is not asked for again, and a write to the record asks once more; the picker re-reads the list when such a refresh returns while it is open, and a reply that arrives after it closed is shown by the next open. A provider that offers nothing selectable keeps a muted row saying why: no models with `dimcode models PROVIDER --refresh` when the daemon says its models can be listed, no models with `provider.models.create` when it cannot, an unreadable catalog, or how many models are unavailable, disabled, or both, and `dimcode models PROVIDER` for the details. An explicit pick sends the saved effort/tier preferences for that exact `{providerId, modelId}` with `session.create` or `session.model.update`, then saves the scoped default with `provider.select`. The adopted model stays in effect even when saving the default fails; the failure is reported as a notice. Unsupported saved preferences are reported and retained for the user to edit or clear; there is no silent fallback. When the daemon advertises `collaboration.model_policy`/`collaboration.set_model_policy`, a "Subagent models" row chooses Agent choice, Inherit requesting branch or a fixed model (from the same model list) for new Collaboration members; it writes only `collaboration.set_model_policy` with the revision it read, and after a revision conflict the next choice rereads before saving ([subagent model policy](../../docs/design/subagent-model-policy.md#5-native-cli-interaction)) |
| `/lines` | Lists `main` and every line under the line it was opened from, ordered by where it was cut and labelled `after message N`, each with its state (working, waiting, ended itself, ended by an operation, creation failed); while a line is shown the composer's bottom border names its path from `main` (`line main ▸ b-91 · after message 4`); Enter shows the selected line's conversation in place of `main`'s, and the composer, `!command` and the interrupt then address that line; `x`, then `y`, ends the selected line through `session.branch_end`, refused on `main` |
| `/timeline` | Lists the shown line's user messages, oldest first, numbered, each with the first text of its answer, the newest selected. Typing does not filter: `e`, `q`, `b` and `f` act on the selected message as on a pick. `Enter` jumps: fullscreen picks the message (drawing from it first when it is earlier history), so the pick keys apply; scrollback draws a message it has not drawn yet, and everything after it up to what is drawn, as an earlier-history page, and otherwise says `/tui` can jump to it ([timeline UI §4.6](../../docs/design/timeline-ui.md#46-timeline-jumping-to-a-message)) |
| `/branch [TEXT]`, `/fork [TEXT]` | As `b` and `f` on a picked message, at the shown line's newest turn end: `/branch` puts `TEXT` in the composer as the new line's opening input; `/fork` asks, then forks `main` there with `TEXT` in the fork's composer |
| `/usage` | Reports main's context window from `session.context.usage`: estimated input tokens against the window, the output allowance and the compaction threshold; before a model has answered it says so, and a failed call is reported alone. With the reading it appends the cache read share of the last `model/usage` request on `main` (` · last request cached {percent}`), rounded down: `100%` only when every prompt token was a cache read, one decimal between `99%` and `100%`, an integer between `1%` and `99%`, `<1%` for a positive share below one percent, and `0%` for a request that reported no cache reads; the prompt token count includes cache writes, so a request that only writes the cache reads `0%`. The segment appears only once `main`'s history has finished loading, so a page that is not the last never reports its older request; after that it shows the newest request, whether it came from the history or a live event. It is absent when that request reported no cache read count or its counters cannot produce a share (cache reads above prompt tokens, or a zero prompt) |
| `/compact` | Asks the shown line to compact its context with `session.context.compact`, during a run too. The matching Timeline row reports the work and its terminal outcome once; the RPC reply adds no second completed, skipped, failed or cancelled notice. Failures of the call itself and invalid replies are reported separately. A daemon without the method says it does not support context compaction |
| `/rename TITLE`, `/rename --clear` | Sets, or clears, the shown Session's manual title with `session.rename` and reports the stored title or that the Session is untitled. Choosing `/rename` from the palette fills `/rename ` and waits; a bare `/rename` is refused with a notice and sends nothing, so `--clear` is the way to clear it without a title. The text is a command: it is never queued or submitted, also while a turn runs. With no Session shown it says so, and a Session that does not serve `session.rename` (`unknown_method`) reports that instead |
| `/goal [OBJECTIVE \| set OBJECTIVE \| edit OBJECTIVE \| pause \| resume \| clear]` | Reads and writes the goal of the shown line through `session.goal.*`. The command's first line names the call, the line and the status in force, the objective follows it, and `/goal` alone prints the goal the line's events give; `edited` is the form that found a live goal and changed only its objective, and a typed objective after a finished goal starts a new one. While the shown line follows a goal the status area carries `◎ goal active` or `◎ goal paused`. A daemon whose Session lists no `session.goal.set` says Goal is not available and calls nothing ([Goal](#goal)) |
| `/effort VALUE` (`/reasoning`), `/fast on\|off`, `/agent ID` | Change the Session's reasoning effort, service tier or agent; without a value they list the choices for the adopted model. `/fast` is listed in the palette only while the adopted model's catalog entry advertises priority/default service tiers |
| Paste of local file paths | An image the model takes becomes an `[Image #N]` token sent as an image block; another file is inserted as its path, and so is an image the model does not take. At most 20 images, a group all or nothing |
| `Ctrl+V` | Reads the native clipboard off the interface thread: files, then an image, then text. `Enter` waits until the read is in |
| A paste of 4 lines or 201 characters or more | Stays a `[... N lines]` token; on send it is stored with `attachment.put` and referenced as `[Pasted text: PATH (N lines)]` |

A conflicting connection edit keeps its fields until the user confirms a retry:

```text
+ Connect <provider ID> ---------------------------------------+
|   Base URL: http://draft.test/v1                             |
|   API Key:  ********                                        |
| > Test + apply                                              |
|   Error: Provider changed; review the retained draft...      |
+ Ctrl+R reread | Enter activate | Esc back -------------------+
```

The account directory follows the entry the CLI was started from, and the user never sets it: the npm launcher passes its package name as `DIMCODE_NPM_PACKAGE`, a standalone binary goes by its command name. `dimagent` signs in at `https://dimagent.com`; `dimcode`, `dim` and any other entry at `https://dimagent.cn`. `dimcode login PROVIDER` uses the same directory unless `--directory URL` is given. Without the account plugin, `/login` says so.

In both TUI modes, authorization links stay in the sign-in dialog, not the transcript.
The complete URL wraps inside the dialog's continuous border. To copy a wrapped
link, use the terminal's rectangular selection to exclude the border and its own
copy command, without a CLI clipboard API.
In short windows the URL preview ends with `…` and a shortened-link hint; the
callback field and submission action remain visible. Enlarge the terminal to
select the complete URL. Both the OSC 8 target and Enter always open the full URL.
Mouse reporting is temporarily disabled while the link is shown, then restored to
the configured setting when the dialog leaves that step. The link carries its full
OSC 8 target; Windows Terminal opens it with `Ctrl+click`. The focused link also
opens with `Enter`. There is no copy button or automatic clipboard write.
Device sign-in shows its pairing code separately from the verification link.
Windows opens authorization links with the system URL handler, without PowerShell.
If the browser cannot open, the link and callback input remain available.

```text
╭──────────────────────────────────────────────────────╮
│ DimAgent OAuth                                       │
│                                                      │
│   Browser login                                      │
│ https://dimagent.cn/oauth/authorize?state=example&r  │
│ edirect_uri=http%3A%2F%2Flocalhost%3A54321%2Fauth%2F │
│ callback                                             │
│   Callback URL: (paste here)                         │
│   Submit callback                                    │
│                                                      │
│ Select to copy · Enter opens link                    │
╰──────────────────────────────────────────────────────╯
```

```text
> /login
╭──────────────────────────────────────────╮
│ DimAgent OAuth                           │
│                                          │
│   Not signed in                          │
│                                          │
│ › Login with DimAgent                    │
│   Enterprise login                       │
│   Device login                           │
│                                          │
│ ↑↓ move · enter activate · esc close     │
╰──────────────────────────────────────────╯
        │ Device login
        ▼
╭──────────────────────────────────────────╮
│ DimAgent OAuth                           │
│                                          │
│   Device login                           │
│   Enter code:    ABCD-EFGH               │
│ https://dimagent.cn/device               │
│   Restart device login                   │
│   Waiting for authorization              │
│                                          │
│ Select to copy · Enter opens link        │
╰──────────────────────────────────────────╯
        │ completed
        ▼
Connected dimcode-api-oauth → /models opens
```

A server whose state becomes `auth_required` is reported once with the `/mcp login` hint.
A draft with images or a stored paste is submitted as ordered `user` content; when a paste
cannot be stored, nothing is sent and the notice names the paste; the cleared draft is not restored.

### Management surfaces

Skills, Hooks and the other management areas reach the terminal UI through one structure rather
than one per feature ([management surfaces](../../docs/design/cli-commands.md#management-surfaces)):

- an area adapter (`crates/dim-cli/src/management/`) talks to the daemon over the client
  connection, turns its owner's reply into one view at that boundary, performs the area's
  actions, and refuses an area whose methods the daemon does not advertise;
- the panel (`crates/dim-cli/src/ui/management_panel.rs`) draws every area on the shared list
  panel, grouped by the source heading the owner reports, and an entry whose owner reports no
  source goes under the area's own heading; the selected entry's detail, its states and the last
  outcome are drawn below the list;
- both surfaces reach the worker through one request and one reply on the session port,
  `Command::Management(ManagementRequest)` and `Update::Management(ManagementReply)`;
- `dimcode <area> <verb>` calls the same adapter over its own connection, so a subcommand needs
  no Session.

Both surfaces compose their text in the interface language ([`/language`](#terminal-ui-modes)).
The session starts in the language the interface chose and switches on every `/language` save,
so a switch shows from the next reply. A subcommand uses the saved language, else the system's.

The keys are one per action kind in every area, and each keeps one meaning: `Enter` or `Space`
toggles, `Ctrl+E` opens a document in the external editor, `Ctrl+R` re-reads the area or asks its
owner to rescan, `Ctrl+N` adds, `Ctrl+X` removes, `Ctrl+T` runs, `Tab` inspects, and `Esc`
closes. A kind is offered only where the area's method table and the owner's view have one, so
an area with no run action never names `Ctrl+T`. An action key is a Ctrl chord, or a key the
filter never takes, because the filter takes every plain letter. The hint names the area's
kinds plus the selected entry's, on the bottom border or, when that cannot hold it, in rows
above the border wrapped by width so no key or action is cut; reload and add act on the area, so
they work with no entry selected: reload retries a failed read, and add opens on an empty area.
A remove asks first, `Remove LABEL? Enter confirm · Esc cancel`, and any other key cancels;
`dimcode <area> remove ID` takes the id on the command line as the intent and does not ask.
Inspect replaces the list with the title and lines the owner returned and scrolls them with the
arrows and PageUp/PageDown; the first `Esc` returns to the list and the next closes the panel.
The Skills and Hooks panels read and write the Session's workspace, not the directory the
client started in; the Scheduler's jobs are the host's, and only a new job's `cwd` defaults from
the Session. A panel holds no area's vocabulary: an area's entries, kinds, documents and detail
arrive from its adapter.

The areas below use it, over the same kinds:

- Skills (`/skills`, `dimcode skills ...`) lists and toggles skills.
- Hooks (`/hooks`, `dimcode hooks ...`) adds the user document's edit.
- The Scheduler (`/scheduled`, `/schedule`, `dimcode scheduled ...`) introduces add, remove, run and inspect, a job per entry from `schedule.list`, with `schedule.set_enabled`, `schedule.save` for the edit and the create, `schedule.delete`, `schedule.run_now` and `schedule.history`. Its subcommands are `dimcode scheduled list`, `enable ID`, `disable ID`, `edit ID`, `set ID PATH`, `add PATH`, `remove ID`, `run ID` and `history ID`.
- Command rules (`/permissions`'s Command rules row, `dimcode permissions ...`) is a rule per entry from `permission.rules` with `Ctrl+R`, `Ctrl+N` and `Ctrl+X`, writing through `permission.set_rules`; its subcommands are `dimcode permissions list`, `add PATH` and `remove 'DECISION MATCH COMMAND'`.

The external editor is `$VISUAL`, then `$EDITOR`, then `vi` (`notepad` on Windows), run on Unix
through `sh -c '<editor> "$@"' <editor> <file>` as git does, so a quoted path with a space
works. Each edit gets its own file, `<home>/edit/<area>-<unique>.json`: created exclusively,
mode 0600 in a 0700 directory, so a kept draft is never overwritten and two clients editing one
area never share a file. The terminal UI releases raw mode and the alternate screen while the
editor runs. A successful write, or an unchanged draft, removes its file. A write the owner
refuses keeps it and the message names it; a refused editor exit (`:cq`, a non-zero status)
writes nothing and does the same. `Ctrl+E` opens the user's text again from that file, unless
the owner's refusal was a conflict: then the area is read again and the next edit opens the
current document at its new revision. `dimcode <area> set PATH` (`-` for stdin) is the same
write without an editor, and `dimcode <area> add PATH` the same shape for a create.

```text
/scheduled ─► Command::Management { area, List } ──► adapter ──► schedule.list
                                                        │
Panel  ◄── Update::Management { area, report } ◄────────┘  groups, entries, states, actions

Enter ────► Command { area, Toggle { id, enabled } } ─► schedule.set_enabled ─► schedule.list
Ctrl+E ───► external editor ─► Command { area, Save { id, expectedRevision, document } } ─► schedule.save
Ctrl+N ───► Command { area, Add { document } } ─► schedule.save
Ctrl+X ───► confirm ─► Command { area, Remove { id } } ─► schedule.delete
Ctrl+T ───► Command { area, Run { id } } ─► schedule.run_now ─► note: "Run nightly: completed."
Tab ──────► Command { area, Inspect { id } } ─► schedule.history ─► Detail { title, lines }
```

## Accounts

`dimcode login [PROVIDER]`, `dimcode account PROVIDER` and `dimcode logout PROVIDER` work on every Provider whose `provider.management` entry carries an `account` descriptor ([Provider management §3.1](../../docs/design/appserver/provider-management.md#31-read-models)). The descriptor's `namespace` names the methods (`<namespace>.login.begin` for device, `<namespace>.login.browser.begin`, `.login.browser.complete`, `.login.status`, `.login.cancel` and `<namespace>.logout`), and its `loginMethods` say which flow a Provider offers. No Provider id is compared. Without a PROVIDER, `login` lists the described accounts; `--help` lists the forms without starting a daemon.

The `account` namespace is the Dim account, a [managed provider plugin](../dim-app-server/README.md#managed-provider-plugins) compiled into the account build of `dimcode` (built with `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` set); no Node runtime, plugin package or extraction is involved. Its calls also carry `providerId` and the account directory. Browser login opens the authorization URL when desktop integration is available, and also prints it. Paste the callback URL into the hidden prompt; `--redirect-uri URI` supplies the provider's registered redirect address. The default is Dim's registered `http://localhost:54321/auth/callback`. No local callback listener is started. The server validates state and performs the exchange. EOF, `Ctrl+C` or expiration cancels an uncompleted attempt.

```bash
target/debug/dimcode --home /path/to/development-home \
  login dimcode-api-oauth --directory https://account-directory.example
```

The other namespaces are native OAuth accounts: `account.codex` signs in by browser and `account.grok` by device code. Their calls carry no provider target, and their browser redirect is the owner's own, so the callback is always pasted.

```bash
dimcode --home /path/to/plugin-home login openai-codex
dimcode --home /path/to/plugin-home login grok-build --device
dimcode --home /path/to/plugin-home account openai-codex
dimcode --home /path/to/plugin-home logout openai-codex
dimcode --home /path/to/plugin-home models PROVIDER --refresh
dimcode --home /path/to/plugin-home --provider PROVIDER --model MODEL_ID
```

Device login prints the verification address and device code, then polls the attempt. `Ctrl+C` cancels it. Account output is a non-secret state view; the CLI never requests token/credential lease APIs. An already accepted browser exchange is owned by the plugin: if the waiter leaves during settlement, inspect it with `account PROVIDER` before starting another login.

A build without the Dim account provider says so from `account`, `login` and `logout`: the Dim account is unavailable and an API-key Provider can be used instead. A Provider the daemon reports without an `account` descriptor does not offer sign-in, and the command says that instead.

The terminal UI's `/connect` row for a described account opens the same flow: a signed-out account starts its only flow, and a signed-in one shows `Signed in` and a `Sign out` entry. A row whose namespace is `account` opens `/login`'s Dim panel (identity, plan, usage, enterprise email) instead.

`models PROVIDER` reads the cached catalog; `--refresh` obtains and persists a fresh catalog through the provider. Each line contains the model ID, display name and `available`, `disabled`, `unavailable` or `undeclared`, separated by tabs; `undeclared` means no source declares the model's context window or maximum output, either of which keeps it from being selected. `--set-limits MODEL --context-window N --max-output-tokens N` declares both for one model, and `--set-missing-limits --context-window N --max-output-tokens N` declares them for every model the list marks `undeclared` — an enabled model whose missing limits are its only blocker, so a disabled model or one unavailable for another reason is not written; both named values are required with either form, both write through `models.edit` at the current record version, and both print the catalog afterwards. When a listed model still needs a window, the command names the declaration below its rows. Credential/account fields and private failure details are not printed. A missing cached catalog gives sign-in and refresh guidance, and a cached catalog that holds no models is named on standard error with the refresh command that fills it; a `--refresh` that lists nothing says the provider listed none. Select an available model with `--provider PROVIDER --model MODEL_ID` for a new session.

An existing shared daemon of the CLI's own version is reused. A daemon of another version that this executable would start (one left running across an upgrade) is stopped with verified shutdown and replaced, which disconnects other clients attached to it; one without verified shutdown is reported for `dimcode daemon stop` from its matching CLI. A `--server PATH` daemon is reused whatever its version. Plugin paths apply when starting a new product daemon; use an isolated home when changing a plugin/runtime configuration. Explicit `--model-config` and account plugin options cannot be combined.

The repository root `./dev` prepares the local provider package, builds incrementally and restarts only its development daemon. See [local development](../../docs/architecture/development.md) for setup and source selection.

```bash
dimcode --home /path/to/plugin-home daemon stop
```

`daemon stop` authenticates an existing endpoint for the selected home without spawning a server or creating a missing home. It requires `daemon.shutdown.completed`, requests normal shutdown, and waits up to 30 seconds for a successful cleanup-completion response followed by connection closure. EOF alone, cleanup errors and timeout report failure without force-killing the process. An older daemon without the feature is left running; stop it using its matching CLI before upgrading. Account and session files remain available for restart. This command accepts a daemon with a different model profile and does not read model/plugin files, so configuration edits cannot prevent an explicit stop. It never removes or waits on a replacement endpoint; successful cleanup includes releasing the original daemon's storage and listener ownership.

## Web workspace

`dimcode web` holds the daemon's local web entry open, `/web` toggles the daemon-owned switch, and both serve the [Web workspace](../../docs/design/web-workspace.md) frontend, which is not inside the executable. `dimcode web --off` clears the switch without disconnecting a foreground holder. The CLI finds it, in this order, and names it to the daemon it starts: the directory in `DIMCODE_WEB_ASSETS` (the npm wrapper sets it to the installed optional dependency `dimcode-web`), `web/` beside the executable's `bin/` (the native archive `dimcode-web-VERSION.tar.gz` unpacked next to it), then `<home>/web`. `dimcode web install` downloads this version's `dimcode-web` from the npm registry (`DIMCODE_NPM_REGISTRY` names a mirror), checks it against the registry's integrity and puts it in `<home>/web`, which a running daemon started without another source serves from its next open; running it again after an upgrade installs the matching frontend, and it names a frontend in an earlier source that still comes first; a build the release flow did not produce has no published frontend, so it refuses before reading the registry. Without one, `dimcode web` (exit status 1) and `/web` say to run it, or, in a build the release flow did not produce, to point `DIMCODE_WEB_ASSETS` at a frontend built from the same source or build the App Server with the embedded-web feature; a daemon started before a frontend appeared in another source is restarted with `dimcode daemon stop`; a frontend recorded for another App Server protocol version (`web/dimcode-web.json`) is refused with `web_assets_incompatible`, and the CLI says how to provide a matching one.

When the daemon keeps a UI registry (`ui.attach` in `hello`), the interface registers itself after every `session.attach` as `kind: tui`; a `ui.shutdown` notification for its id draws one last frame with the reason, then the interface exits 0 and the Session carries on. Any number of interfaces view one Session. When the daemon offers `ui.leaf.2`, interactive `dimcode` is itself a UI leaf (`crate::leaf`): it creates or resumes the Session, mounts `LeafNode` in its client tree (its `LocalId` `ui-<id>`, and a socket `ui-<8 hex>` in the private native home, as long as `daemon.sock`, that accepts one connection) and calls the internal `ui.open`; the daemon opens a rutis session with it and replies `{uiId, generation, token}`, and the process then runs its own `Tui` with this invocation's options, attaching with that token. The process ends when the `Tui` leaves (its exit code, or a restart) or when the session ends (exit 0); a `Tui` that fails to load ends it nonzero. rutis reports nothing to the terminal (the root's error sink writes nothing). A daemon without `ui.leaf.2` keeps the direct TUI. Initial product setup without a usable model also uses the existing direct TUI, so `/connect` and `/login` remain available before a Session exists; explicit model targets, `--model-config`, resume and other error codes keep their errors ([Tui §3.6](../../docs/design/plugins/Tui.md#36-ui-registry-and-ui-leaves)).

## Home, transport and output

The default product home is `~/.dimcode`, or `~/.dimcode-alpha` for an alpha build so it never shares state with a release; `DIMCODE_HOME` or `--home PATH` overrides it. Native state and the daemon endpoint/log live in `<home>/rust`. No existing home, auth file or database is automatically migrated. Use an isolated home for development and acceptance.

`<home>/config.json` holds the interface settings and `<home>/input-history.json` the prompt history of each working directory; a write replaces either file whole and never edits it in place. Both are written under an exclusive lock on `<file>.lock` beside the path the writer was given, which then follows that path when it is a symbolic link: the temp file, the rename and the directory sync go beside the resolved file, so a linked `config.json` keeps its link and its target takes the write, and no lock is left in the linked directory. The new bytes go to a synced temp file that is renamed over the file, so two `dimcode` processes sharing a home keep each other's keys and entries, and a reader takes no lock and sees the old file or the new one. `config.json` is written on the thread that changed a setting: a write waits on the lock while another writer holds it and writes once it is released, so contention is never a failed write, and a write that fails (a lock that cannot be taken at all included) is reported to the user and leaves the file alone, since a home whose filesystem refuses locks cannot hold the `dimcode.sqlite` beside it either. `input-history.json` never blocks the interface or a submit: the entry is handed to one writer thread and the submit returns at once, the entry is written under the same lock, and a write that fails is dropped, because history is a recall convenience that is allowed to be lost; the interface waits for the queued entries only as it exits. A `config.json` that exists but does not hold a JSON object is left as it is and the parse error is returned, while an `input-history.json` that does not parse is replaced by one holding the submitted entry. The `.lock` file stays beside the named path, and a write that fails before the rename leaves the old file and removes its temp file.

On Unix, the native directory must be private (`0700`), and the socket must be private and owned by the current user. The CLI validates peer credentials before sending RPC data. On Windows, it uses the existing authenticated `dimcode app-server --connect-pipe` bridge (or `--server PATH --connect-pipe`), which verifies the connected pipe's owner before forwarding any RPC bytes. The client uses v3 protocol capabilities to reject incompatible daemons; it does not remove unknown endpoints or kill another daemon.

The daemon startup wait allows a newly spawned, current-user Unix socket to become private
before connecting. Existing insecure endpoints are rejected immediately. Wrong ownership,
non-socket paths and peer-credential failures remain immediate errors; startup does not retry
arbitrary permission errors.

Noninteractive model text goes to stdout. Session handles, tool status and diagnostics go to stderr. Interactive per-tool approval requires a terminal; in noninteractive mode the operation is denied, the turn is stopped and settled, and exit code `3` is returned. A question also requires a terminal; in noninteractive mode its branch and the turn are stopped and settled, and exit code `3` is returned. Configuration/transport failures do not silently select a mock model or old core.

Requests are limited to 16 MiB of encoded JSON, excluding the NDJSON newline.
The CLI checks the complete request before writing any bytes to the daemon;
JSON escaping can make a prompt exceed this limit even when its original text fits.

| Exit code | Meaning |
| --- | --- |
| `0` | Completed, or normal interactive exit |
| `1` | Invalid configuration, incompatible daemon, login or transport error |
| `2` | Model or turn failed |
| `3` | Interactive approval or answer required |
| `130` | Interrupted noninteractive turn or login |

When a model reaches `max_tokens`, the CLI names its provider/model and asks for
a larger output token limit. The turn fails with exit code `2`; the Session stays
usable. Model request failures are also recorded in the private daemon log with their
Session, turn and request identities.

```text
exec -> [failed] local: Model local/model stopped at max_tokens. Increase the output token limit and retry.
TUI  -> model failed: Model local/model stopped at max_tokens. Increase the output token limit and retry.
ACP  -> stopReason: max_tokens -> next prompt on the same Session
```

## Focused checks

The public CLI flow tests exercise the real native binary and NDJSON boundary. Unix PTY checks cover default-deny approval, questions, discarded typeahead, stopping while a frame is fragmented, prompt recovery and hidden callback input:

```bash
cargo test --locked -p dimcode --test cli_flow
cargo clippy --locked -p dim-cli --all-targets -- -D warnings
```

The production-model flow uses the actual App Server, an API-key environment reference and a local OpenAI-compatible HTTP stream fixture. It checks the production tool list, a real file read reaching the model, and continued input on the same persisted session:

```bash
cargo build --locked -p dim-app-server
DIM_TEST_APP_SERVER="$PWD/target/debug/dim-app-server" \
  cargo test --locked -p dimcode --test cli_flow \
  configured_api_key_model_uses_production_tools_and_restores_the_same_session \
  -- --ignored
```

The permission flows run against the same fixture: `exec --permission full-access` runs a process tool without a question, default `exec` still exits 3, `exec resume --permission` changes the resumed branch, and "always" saves an exact rule after which the identical call runs without asking until `/approvals remove` deletes it:

```bash
DIM_TEST_APP_SERVER="$PWD/target/debug/dim-app-server" \
  cargo test --locked -p dim-cli --test cli_flow permission::real_daemon -- --ignored
```

The daemon-profile check rejects an already running mock or different configured daemon, verifies that its process and endpoint remain intact, then explicitly stops it through the CLI despite a changed invalid model configuration:

```bash
DIM_TEST_APP_SERVER="$PWD/target/debug/dim-app-server" \
  cargo test --locked -p dimcode --test cli_flow \
  real_daemon_profile_conflicts_preserve_the_running_daemon_and_endpoint \
  -- --ignored
```

The npm wrapper can be checked independently with the same PTY input and stop/settle flow. This uses a local platform-package fixture and does not access npm:

```bash
DIM_TEST_NPM_WRAPPER="$PWD/npm/bin/dimcode.mjs" \
  cargo test --locked -p dimcode --test cli_flow \
  unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow \
  -- --ignored
```

The test uses an isolated home and explicitly shuts down only the daemon it started. The external model service is a fixture; it does not spend a real provider's quota. Windows CI runs native clipboard and ConPTY editing flows; compiling another target is not a substitute for them. The named-pipe bridge still requires its separate native acceptance.

Unix socket paths have an operating-system length limit, including the canonical home path. On macOS, `/var` resolves to `/private/var`, which adds bytes. Use a short isolated home such as `/tmp/dim-dev` for development/tests if the daemon reports `path must be shorter than SUN_LEN`; the CLI preserves that failure in `rust/daemon.log`.
