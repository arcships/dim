# Rust CLI

`dimcode` is a thin Rust client for the local App Server. It owns command-line input, scrollback output, approvals, questions and account prompts. The App Server owns model execution, tools, session state and persistence. The npm wrapper exposes the same program as `dim` and `dimcode`.

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

local files -> bounded read/header checks -> user.content -> session.submit
```

The CLI reads and checks all local images before connecting: at most 20 images,
5 MiB of image-file bytes each, and 8000 pixels per side. Format comes from bytes,
including when a filename has another extension. Invalid/truncated headers fail;
pixels are not resized or rewritten. The `image` dependency enables only these
four formats and reads dimensions without decoding pixels. Local symbolic links
are followed, and file type is checked on the opened file. The combined base64
size is checked before connecting; the final encoded request, including its JSON
envelope and text, must still fit 16 MiB.

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

Ctrl+V reaching the CLI reads clipboard files first, then an image, then plain text.
macOS uses AppKit; Windows uses Win32. Linux needs `wl-paste` on Wayland or `xclip` on
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

Help and version commands do not start a daemon or read credentials. This crate is a library; the `dimcode` crate builds the single executable, which runs the CLI and, as `dimcode app-server`, the App Server. The CLI starts its daemon by running its own executable with `app-server`, through a `dimcode-server` link to it in `<home>/rust` so that process lists (`ps`, `pgrep dimcode-server`, Activity Monitor) tell the daemon from the clients that share the executable; `--server PATH` selects an explicit App Server binary instead and runs it by its own name.

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
        { "id": "your-model-id", "config": { "maxTokens": 4096 } }
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
Missing scopes appear below the entry.
It does not print commands, environment values, headers or credentials. `mcp enable ID` and
`mcp disable ID` change the entry that `mcp list` shows for the workspace.

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

The App Server decides each tool call by the branch's preset and the user's command rules; the
CLI selects them through existing methods and never decides a call itself. A preset or approval
decides whether a command runs; [command isolation](#command-isolation) limits what it can
reach once it runs, and neither one widens the other.

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

At the idle prompt, `/approvals` (alias `/permissions`) shows the `main` preset and the user's
command rules. `/approvals PRESET` switches `main`; `full-access` asks `[y/N]` and only `y`/`yes`
confirms. `/approvals allow|deny exact|prefix COMMAND` adds a rule and `/approvals remove N`
removes rule N of the list printed last; both print the new numbered list. A preset or rule list
that changed since it was shown is reported and nothing is written. Child branches created later inherit a switch; agent members keep the
Session's starting preset. Rules are user scope and apply in every Session: a deny rule under
every preset, an allow rule only where the preset would ask.

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

## Command isolation

On macOS and Linux, commands the model runs with `exec` start in a sandbox: Seatbelt on macOS,
bubblewrap on Linux. Without a policy file, a command can write only inside the session
workspace, read only the workspace and system directories, and has no network access. The
[defaults table](../../docs/design/host-platform-isolation.md#scope-and-policy) lists each
platform's directories and the environment variables passed through. Approving a command or
choosing `full-access` does not widen the sandbox; only the policy file does.

On macOS only, the default also lets common developer tools run:

- Each command gets its own empty temporary directory as `TMPDIR`, writable and removed after the
  command ends. `/tmp` and your own `TMPDIR` are not granted.
- The `/usr/bin` developer tools (`git`, `python3`, `clang`, `make`) can read the selected
  developer directory: `DEVELOPER_DIR` when the daemon's environment sets it, otherwise the one
  `xcode-select -p` prints. The Command Line Tools work; an Xcode.app selection is not supported.
  On such a Mac, run `xcode-select -s /Library/Developer/CommandLineTools`, start dimcode with
  `DEVELOPER_DIR=/Library/Developer/CommandLineTools`, or turn isolation off.
- Git can read its global configuration: `~/.gitconfig`, and `config`, `ignore` and `attributes`
  in `$XDG_CONFIG_HOME/git` (`~/.config/git` when unset). Each file is granted on its own, so
  `~/.git-credentials`, the `credentials` file beside them, `~/.ssh` and the rest of your home stay
  unreadable. Files pulled in with `include.path` or `includeIf` are not granted, and git then
  fails with `Operation not permitted`; list them in `readOnlyPaths` as shown below.

A denied operation fails inside the command as an ordinary error: `Operation not permitted` on
macOS; on Linux, `No such file or directory` for an ungranted path and `Read-only file system`
for a read-only one. When an isolated command fails with a typical denial, its result tells the
model that isolation may be the cause and names the policy file. The CLI does not display
whether isolation is on; the policy file decides it.

### Policy file

The daemon reads `host-isolation.json` from its home:

| Profile | Policy file |
| --- | --- |
| Product (`dimcode`, `--home PATH`) | `<home>/host-isolation.json`, by default `~/.dimcode/host-isolation.json` |
| API-key configuration (`--model-config`) | `<home>/rust/host-isolation.json`, because this profile starts the daemon with `--home <home>/rust` |

dimcode never creates the file. It is read each time a command starts, so an edit applies to the
next command without restarting the daemon; a running command keeps the policy it started with.
An invalid file makes every command fail with `isolation_config_invalid` until it is fixed.

Only `version` is required, and it must be `1`. Each other field replaces its default:

| Field | Meaning | Default |
| --- | --- | --- |
| `enabled` | `false` runs commands as ordinary processes | `true` |
| `network` | `true` allows network access | `false` |
| `readOnlyPaths` | Directories and files commands can read | The platform's system directories; on macOS also the developer directory and git's configuration files |
| `writablePaths` | Directories commands can read and write | The session workspace; on macOS also each command's `TMPDIR` |
| `environment` | Names of daemon environment variables passed to commands | The list in the defaults table |

Paths must be absolute and must exist; `~` is not expanded. `readOnlyPaths` takes directories
and regular files: a file entry grants that file alone, not the directory around it, and an
entry that is a symlink also grants the file it points to. `writablePaths` takes directories
only. A list replaces its default completely, and `[]` grants nothing, so an explicit
`writablePaths` must name every workspace that should stay writable. A read-only entry cannot be
inside a writable one. Unknown fields are errors.

On macOS a written list also drops the macOS additions above. With `writablePaths`, commands get
no `TMPDIR` of their own; a `TMPDIR` named in `environment` passes through and needs its own
grant. With `readOnlyPaths`, list `/var/select`, the developer directory and git's
configuration files yourself, as in the example below. Without them, git exits with
`Operation not permitted` while `~/.gitconfig` or the XDG `config` exists, and skips `ignore` and
`attributes` with a warning. List each of these files that exists as its own entry:
`~/.gitconfig`; `config`, `ignore` and `attributes` in `$XDG_CONFIG_HOME/git` (`~/.config/git`
when unset); and every file that `include.path` or `includeIf` pulls in. Listing a file that does
not exist makes the policy invalid. Prefer the files to their directory: listing `~/.config/git`
also exposes every other file in it, including a `credentials` file if git keeps one there.

To turn isolation off:

```json
{"version": 1, "enabled": false}
```

Commands then have your user's ordinary access. On Linux without bubblewrap, every command fails
with `isolation_unavailable` until bubblewrap is installed or isolation is turned off.

### Toolchains outside system directories

Homebrew, rustup, nvm and similar toolchains are not readable by default. Grant them in
`readOnlyPaths`, repeating the system directories because the list replaces them, and list only
paths that exist. On macOS with the Command Line Tools, git's configuration in `~/.gitconfig` and
`~/.config/git/ignore`, Homebrew in `/opt/homebrew`, rustup and nvm:

```json
{
  "version": 1,
  "readOnlyPaths": [
    "/System", "/usr", "/bin", "/sbin", "/Library/Apple", "/private/etc", "/etc",
    "/var/select", "/Library/Developer/CommandLineTools",
    "/Users/you/.gitconfig", "/Users/you/.config/git/ignore",
    "/opt/homebrew",
    "/Users/you/.cargo",
    "/Users/you/.rustup",
    "/Users/you/.nvm"
  ]
}
```

On Linux, start from `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64` and `/etc`, leaving out any that do
not exist, and add directories such as `/home/linuxbrew/.linuxbrew` or `/home/you/.cargo`. Git
reads no global configuration there by default; add `/home/you/.gitconfig` and the other files
above to let it. A tool
that writes caches or locks in its directory needs it in `writablePaths` instead, next to the
workspace, and downloads need `"network": true`. A granted directory exposes everything in it; `~/.cargo`, for example,
can hold registry tokens.

### Not covered

- The file tools (`read`, `write`, `edit`, `glob` and `grep`) run inside the daemon, not in the
  sandbox. Presets and approvals govern them, and
  [#294](https://github.com/arcships/dim/issues/294) tracks limiting the paths they write.
- Commands you type with `!` or `!!` run as ordinary processes.
- Windows has no sandbox: commands run as ordinary processes and the file is not read.
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
 Space toggle · Enter next · Tab switch · Esc decline
```

Number keys select choices, Space toggles a multi-select choice, and Tab/Shift+Tab
or Left/Right changes question tabs. Enter on Confirm sends one answer per question;
offered labels and custom text remain separate. The custom row exists only when
`allowCustom` is true. Esc declines the whole call. An existing approval or question
keeps its prompt until handled; leader prompts preempt a read-only member detail.
Member questions never open a response control in the leader UI.

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
 ╭──────────────────────────────────────── provider / model ─╮      dock: queue, composer
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
| `Tab` / `Shift+Tab` | Pick older / newer user messages (`Up` / `Down` while one is picked); past the newest the pick ends. `Esc` ends it, `e` puts the message in the composer to edit and send again |
| `/`, `Ctrl+O`, `Ctrl+P` | Open the command palette centred over the dimmed transcript, with its own query row; a draft set aside by `Ctrl+O`/`Ctrl+P` returns when the palette closes |
| `Home` / `End` | With an empty composer: scroll to the top / bottom of the transcript |
| `Esc` | Interrupt the run |
| `Ctrl+C` | Clear the draft, then exit on a second press; a busy session asks for confirmation instead of being interrupted |

The composer box: no prompt mark or placeholder, the model at the right of its top border, and in its bottom border the run status, the status slot (a notice, `esc interrupt`, `permission required`) and a non-default approvals mode at the left, the working directory and git branch at the right. Fullscreen has no status row under it; scrollback keeps its status row and `›` prompt.

`/models` uses Ctrl+X to clear the highlighted model's saved reasoning/tier preferences with the displayed record version; Enter adopts it separately. For noninteractive recovery, run `dimcode models PROVIDER --clear-preferences MODEL`. A concurrent change fails and requires a reread/retry; preferences are never silently discarded.

Pickers (`/resume`, `/models`, `/connect`, `/login`, `/approvals`, `/theme`, `/language`, `/settings`) open as centred boxes too; the mouse wheel moves their selection. Approval and question prompts, and the exit confirmation, take the composer's place until they are answered, in both modes: there is nothing to type into meanwhile. The live tail above (the tool group waiting on the answer, a running thought) stays where it is. Every panel is framed the same way: the title in the top border, the keys in the bottom one (`╰── Enter select · Esc close ─╯`, keys in the keybinding colour), and a filterable list has a `> query` row between blank rows instead of a count; the query is a line editor like the composer's (cursor keys, Home/End, Delete, pastes).

```text
 ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓    picked message: the box
 ┃ ┃ Explain this project                                   ┃    takes the blank rows
 ┗━e to edit━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛    around it, nothing moves
 The project …        ╭─ Command Palette ─────────────────╮
                      │                                    │    palette: own query row,
                      │ > th                               │    transcript dimmed behind,
                      │                                    │    the selected row on an
                      │▓ view  /theme  switch theme ▓▓▓▓▓▓▓│    accent bar
                      │   app  /exit   exit   Ctrl+C Ctrl+C│
                      ╰────────────────────────────────────╯
```

The palette lists what the interface runs: each row reads category, the action (a `/command` and its description, or an action a key reaches), and at the right edge the key that runs it (`Esc` interrupt, `Ctrl+S` steer, `Home` / `End` scroll, `Tab` pick a message, `Ctrl+C Ctrl+C` exit). An action that cannot run now (no session yet, nothing running, no queued message) is faint and marked `(unavailable)`; fullscreen-only actions are left out of the scrollback palette. Typing ranks the matches: the exact name, a name prefix, an alias prefix, a prefix of a name part (`reload-skills` for `sk`), a name containing the query, a description word starting with it, then three or more letters in order through the name (`stg` for `settings`); a match inside a description word does not count.

`/settings` gathers the interface settings: theme and language (each opens its picker and comes back), the startup form (`tuiMode`), tool details (`tuiToolDetailsExpanded`: expanded by default in both modes; collapsed shows each tool group as its header and folds thoughts, and `/tools` flips it too), the fullscreen mouse (`tuiMouseMode`: off hands selection and the wheel to the terminal), notifications (`tuiNotifyOnComplete`, on by default), tool approvals and the model. Switches flip with `Enter`, `←`/`→` or `Space` and are saved to `config.json` at once.

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

In the default theme (`System`) every colour is the terminal's own default foreground, a slot of its 16-colour palette (`ansi:N`) or the faint attribute, so it follows the terminal's colour scheme on dark and light backgrounds alike. Yellow is the accent (titles, the selected row, inline code), green the user's messages and successes, blue first-level and cyan second-level headings, red errors; muted text is faint, table rules bright black, and code is highlighted in the terminal's base colours (keywords and types blue, calls cyan, numbers yellow, strings green).

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
100 messages and its boundary. Earlier messages remain available through
`/history`; a draft typed during paging stays editable and survives completion.

```text
> paged-message-051
...
> paged-message-150
--- resumed paged · 100 messages · earlier history: /history ---
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
| `!command` / `!!command` | Runs through `shell.run` in record or private mode; its progress streams under the composer, `Esc` or `Ctrl+C` sends `shell.stop`. A recorded run comes back as the Session's input event and is drawn as a shell block, also after resume; a private run, or one that could not be recorded, is drawn only here |
| `/mcp`, `/mcp list` | MCP servers with their authorization, then the Session's server states |
| `/mcp login [--no-browser] ID` | Starts sign-in, opens the browser unless `--no-browser` or SSH, and catches the callback on a loopback listener when the redirect port is free; `/mcp callback URL` completes it with a pasted callback instead. A client secret needs hidden input, so `--client-secret` points to `dimcode mcp login ID --client-secret` |
| `/mcp logout ID` | Signs out and reports whether the token was revoked |
| `/login` | Opens the Dim sign-in panel. It shows the account from `account.status` as a status line; signed out, its entries are browser, enterprise-email or device sign-in through `account.login.*` at the entry's account directory (below), each entering its own flow (`Esc` after a failure returns to the entries); signed in, it shows the account's name and email, its plan, credits, token windows and feature meters from `account.usage` (`Ctrl+R` asks again; a plugin without the method shows why), and its entries open `/models`, open the plan page at the account directory's `/console/subscription` in the browser, or sign out with `account.logout`, after which the panel shows the sign-in entries again. Browser sign-in redirects to `http://localhost:54321/auth/callback` (`--redirect-uri` overrides it), catches the callback on a loopback listener when that port is free and opens the authorization URL (neither over SSH); a callback URL pasted in the panel completes it too; device sign-in shows the code and polls the attempt every second. `Esc` closes the panel and cancels a pending attempt. On success the catalog is refreshed with `provider.refresh` and `/models` opens |
| `/connect` | Reads safe connection snapshots with `provider.management`. `Test + apply` probes the unsaved draft, then updates only changed fields at the form's original version with `provider.update`; unsupported model listing still permits saving and reports that the key was not tested. Other probe failures keep the draft. `Disconnect` atomically clears credentials and disables the record; Enable/Disable update only that flag. `Add custom provider` probes a Name, Base URL and API Key draft, then calls `provider.create` with the form's retained UUID and discovered defaults. IDs never derive from names. Failures retain edits; rereads update the safe status/version and require confirmation before retrying. `Ctrl+R` rereads without discarding the draft. Fields and filters use the composer's line editor. Account sign-in retains its existing owner |
| `/models` | Lists enabled, executable models from `provider.list`/`provider.catalog`. An explicit pick sends the saved effort/tier preferences for that exact `{providerId, modelId}` with `session.create` or `session.model.update`, then saves the scoped default with `provider.select`. The adopted model stays in effect even when saving the default fails; the failure is reported as a notice. Unsupported saved preferences are reported and retained for the user to edit or clear; there is no silent fallback. When the daemon advertises `collaboration.model_policy`/`collaboration.set_model_policy`, a "Subagent models" row chooses Agent choice, Inherit requesting branch or a fixed model (from the same model list) for new Collaboration members; it writes only `collaboration.set_model_policy` with the revision it read, and after a revision conflict the next choice rereads before saving ([subagent model policy](../../docs/design/subagent-model-policy.md#5-native-cli-interaction)) |
| `/usage` | Reports the branch's context window from `session.context.usage`: estimated input tokens against the window, the output allowance and the compaction threshold; before a model has answered it says so |
| `/effort VALUE` (`/reasoning`), `/fast on\|off`, `/agent ID` | Change the Session's reasoning effort, service tier or agent; without a value they list the choices for the adopted model. `/fast` is listed in the palette only while the adopted model's catalog entry advertises priority/default service tiers |
| Paste of local file paths | An image becomes an `[Image: name]` token sent as an image block; another file is inserted as its path. At most 20 images, a group all or nothing |
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

```text
> /login
╭─ DimAgent OAuth ─────────────────────────╮
│  Signed in: 3f9c…                         │
│  › Login with DimAgent                    │
│    Enterprise login                       │
│    Device login                           │
│                                           │
│  ↑↓ move · enter activate · esc back      │
╰───────────────────────────────────────────╯
        │ Device login
        ▼
│  [ ] Browser mode / [x] Device mode       │
│  Enter code ABCD-EFGH at https://…        │
│  Waiting for authorization                │
        │ completed
        ▼
Connected dimcode-api-oauth → /models opens
```

A server whose state becomes `auth_required` is reported once with the `/mcp login` hint.
A draft with images or a stored paste is submitted as ordered `user` content; when a paste
cannot be stored, nothing is sent and the notice names the paste; the cleared draft is not restored.

## Accounts

The Dim account is a [managed provider plugin](../dim-app-server/README.md#managed-provider-plugins) compiled into the account build of `dimcode` (built with `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` set); no Node runtime, plugin package or extraction is involved. The public build supports API-key providers only.

```bash
target/debug/dimcode --home /path/to/development-home \
  login dimcode-api-oauth --directory https://account-directory.example
```

The provider advertises browser/device methods through `account.providers`. Browser login opens the authorization URL when desktop integration is available, and also prints it. Paste the callback URL into the hidden prompt; `--redirect-uri URI` supplies the provider's registered redirect address. The default is Dim's registered `http://localhost:54321/auth/callback`. No local callback listener is started. The server validates state and performs the exchange. EOF, `Ctrl+C` or expiration cancels an uncompleted attempt.

```bash
dimcode --home /path/to/plugin-home login PROVIDER \
  --directory https://account-directory.example --device
dimcode --home /path/to/plugin-home account PROVIDER
dimcode --home /path/to/plugin-home logout PROVIDER
dimcode --home /path/to/plugin-home models PROVIDER --refresh
dimcode --home /path/to/plugin-home --provider PROVIDER --model MODEL_ID
```

Device login prints the verification address and device code, then polls the attempt. `Ctrl+C` cancels it. Account output is a non-secret identity view; the CLI never requests token/credential lease APIs. An already accepted browser exchange is owned by the plugin: if the waiter leaves during settlement, inspect it with `account PROVIDER` before starting another login.

`models PROVIDER` reads the cached catalog; `--refresh` obtains and persists a fresh catalog through the provider. Each line contains the model ID, display name and `available`, `disabled` or `unavailable`, separated by tabs. Credential/account fields and private failure details are not printed. A missing cached catalog gives sign-in and refresh guidance. Select an available model with `--provider PROVIDER --model MODEL_ID` for a new session.

An existing shared daemon of the CLI's own version is reused. A daemon of another version that this executable would start (one left running across an upgrade) is stopped with verified shutdown and replaced, which disconnects other clients attached to it; one without verified shutdown is reported for `dimcode daemon stop` from its matching CLI. A `--server PATH` daemon is reused whatever its version. Plugin paths apply when starting a new product daemon; use an isolated home when changing a plugin/runtime configuration. Explicit `--model-config` and account plugin options cannot be combined.

The repository root `./dev` prepares the local provider package, builds incrementally and restarts only its development daemon. See [local development](../../docs/architecture/development.md) for setup and source selection.

```bash
dimcode --home /path/to/plugin-home daemon stop
```

`daemon stop` authenticates an existing endpoint for the selected home without spawning a server or creating a missing home. It requires `daemon.shutdown.completed`, requests normal shutdown, and waits up to 30 seconds for a successful cleanup-completion response followed by connection closure. EOF alone, cleanup errors and timeout report failure without force-killing the process. An older daemon without the feature is left running; stop it using its matching CLI before upgrading. Account and session files remain available for restart. This command accepts a daemon with a different model profile and does not read model/plugin files, so configuration edits cannot prevent an explicit stop. It never removes or waits on a replacement endpoint; successful cleanup includes releasing the original daemon's storage and listener ownership.

## Web workspace

`dimcode web` holds the daemon's local web entry open, `/web` toggles the daemon-owned switch, and both serve the [Web workspace](../../docs/design/web-workspace.md) frontend, which is not inside the executable. `dimcode web --off` clears the switch without disconnecting a foreground holder. The CLI finds it, in this order, and names it to the daemon it starts: the directory in `DIMCODE_WEB_ASSETS` (the npm wrapper sets it to the installed optional dependency `dimcode-web`), `web/` beside the executable's `bin/` (the native archive `dimcode-web-VERSION.tar.gz` unpacked next to it), then `<home>/web`. `dimcode web install` downloads this version's `dimcode-web` from the npm registry (`DIMCODE_NPM_REGISTRY` names a mirror), checks it against the registry's integrity and puts it in `<home>/web`, which a running daemon started without another source serves from its next open; running it again after an upgrade installs the matching frontend, and it names a frontend in an earlier source that still comes first. Without one, `dimcode web` (exit status 1) and `/web` say to run it; a daemon started before a frontend appeared in another source is restarted with `dimcode daemon stop`; a frontend recorded for another App Server protocol version (`web/dimcode-web.json`) is refused with `web_assets_incompatible`, and the CLI says to run `dimcode web install`.

When the daemon keeps a UI registry (`ui.attach` in `hello`), the interface registers itself after every `session.attach` as `kind: tui`; a `ui.shutdown` notification for its id draws one last frame with the reason, then the interface exits 0 and the Session carries on. Any number of interfaces view one Session. When the daemon offers `ui.leaf`, interactive `dimcode` is itself a UI leaf (`crate::leaf`): it creates or resumes the Session, mounts a rutis node in its client tree (a socket `ui-<8 hex>` in the private native home, as long as `daemon.sock`, that accepts one connection, and `PeerPlugin` with `host { Tui }`) and calls the internal `ui.open`; the daemon links to it and hosts `Tui` there with this invocation's options. The process ends when the `Tui` leaves (its exit code, or a restart) or when the daemon closes the link (exit 0). An older daemon without `ui.leaf` keeps the direct TUI. Initial product setup without a usable model also uses the existing direct TUI, so `/connect` and `/login` remain available before a Session exists; explicit model targets, `--model-config`, resume and other error codes keep their errors ([Tui §3.6](../../docs/design/plugins/Tui.md#36-ui-registry-and-ui-leaves)).

## Home, transport and output

The default product home is `~/.dimcode`, or `~/.dimcode-alpha` for an alpha build so it never shares state with a release; `DIMCODE_HOME` or `--home PATH` overrides it. Native state and the daemon endpoint/log live in `<home>/rust`. No existing home, auth file or database is automatically migrated. Use an isolated home for development and acceptance.

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
