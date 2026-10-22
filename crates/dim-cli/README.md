# Rust CLI

`dimcode` is a thin Rust client for the local App Server. It owns command-line input, scrollback output, approvals, questions and account prompts. The App Server owns model execution, tools, session state and persistence. The npm wrapper exposes the same program as `dim` and `dimcode`.

`dimcode tui` opens the interactive client. `dimcode exec -p TEXT` runs one prompt
and exits; redirected stdin supplies the prompt when `-p` is absent.
`dimcode exec resume SESSION_ID -p TEXT` continues a persisted Session. Existing
`--prompt`, piped stdin and `--resume` entries remain available.

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
cargo build --locked -p dim-cli -p dim-app-server
target/debug/dimcode --help
```

Help and version commands do not start a daemon or read credentials. The CLI finds `dim-app-server` beside its own executable; `--server PATH` selects an explicit binary.

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

`--cwd PATH` selects the workspace. `--provider ID --model ID` selects a different configured model for a new session. The server validates the selection.

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
CLI selects them through existing methods and never decides a call itself.

| Preset | Reads | File writes | Commands, network, unclassified tools |
| --- | --- | --- | --- |
| `read-only` | run | denied | denied |
| `workspace-write` (product default) | run | run | ask |
| `full-access` | run | run | run |

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

## Scrollback and session recovery

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

## Accounts and private plugin runtime

Public API-key builds require no account plugin or Node runtime. For an installed payload, the server binary is in `bin/`, and the CLI recognizes `plugins/dim-oauth/dim-plugin.json` relative to that directory's parent. It starts the server with the packaged Node binary and the rutis runtime package. Missing or invalid runtime files cause startup failure; there is no runtime npm installation.

For local plugin development, pass all three paths together:

```bash
target/debug/dimcode --home /path/to/plugin-home \
  --plugin-dir /path/to/private-plugin-package \
  --node /path/to/node \
  --js-runtime /path/to/node_modules/@arcships/rutis-runtime \
  login PROVIDER --directory https://account-directory.example
```

The provider advertises browser/device methods through `account.providers`. Browser login opens the authorization URL when desktop integration is available, and also prints it. Paste the callback URL into the hidden prompt; `--redirect-uri URI` supplies the provider's registered redirect address. The default is `http://localhost:1455/auth/callback`. No local callback listener is started. The server/plugin validates state and performs the exchange. EOF, `Ctrl+C` or expiration cancels an uncompleted attempt.

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

An existing shared daemon is reused. Plugin paths apply when starting a new product daemon; use an isolated home when changing a plugin/runtime configuration. Explicit `--model-config` and account plugin options cannot be combined.

The repository root `./dev` prepares the local provider package, builds incrementally and restarts only its development daemon. See [local development](../../docs/architecture/development.md) for setup and source selection.

```bash
dimcode --home /path/to/plugin-home daemon stop
```

`daemon stop` authenticates an existing endpoint for the selected home without spawning a server or creating a missing home. It requires `daemon.shutdown.completed`, requests normal shutdown, and waits up to 30 seconds for a successful cleanup-completion response followed by connection closure. EOF alone, cleanup errors and timeout report failure without force-killing the process. An older daemon without the feature is left running; stop it using its matching CLI before upgrading. Account and session files remain available for restart. This command accepts a daemon with a different model profile and does not read model/plugin files, so configuration edits cannot prevent an explicit stop. It never removes or waits on a replacement endpoint; successful cleanup includes releasing the original daemon's storage and listener ownership.

## Home, transport and output

The default product home is `~/.dimcode`; `DIMCODE_HOME` or `--home PATH` overrides it. Native state and the daemon endpoint/log live in `<home>/rust`. No existing home, auth file or database is automatically migrated. Use an isolated home for development and acceptance.

On Unix, the native directory must be private (`0700`), and the socket must be private and owned by the current user. The CLI validates peer credentials before sending RPC data. On Windows, it uses the existing authenticated `dim-app-server --connect-pipe` bridge, which verifies the connected pipe's owner before forwarding any RPC bytes. The client uses v3 protocol capabilities to reject incompatible daemons; it does not remove unknown endpoints or kill another daemon.

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
cargo test --locked -p dim-cli --test cli_flow
cargo clippy --locked -p dim-cli --all-targets -- -D warnings
```

The production-model flow uses the actual App Server, an API-key environment reference and a local OpenAI-compatible HTTP stream fixture. It checks the production tool list, a real file read reaching the model, and continued input on the same persisted session:

```bash
cargo build --locked -p dim-app-server
DIM_TEST_APP_SERVER="$PWD/target/debug/dim-app-server" \
  cargo test --locked -p dim-cli --test cli_flow \
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
  cargo test --locked -p dim-cli --test cli_flow \
  real_daemon_profile_conflicts_preserve_the_running_daemon_and_endpoint \
  -- --ignored
```

The npm wrapper can be checked independently with the same PTY input and stop/settle flow. This uses a local platform-package fixture and does not access npm:

```bash
DIM_TEST_NPM_WRAPPER="$PWD/npm/bin/dimcode.mjs" \
  cargo test --locked -p dim-cli --test cli_flow \
  unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow \
  -- --ignored
```

The test uses an isolated home and explicitly shuts down only the daemon it started. The external model service is a fixture; it does not spend a real provider's quota. Windows CI runs native clipboard and ConPTY editing flows; compiling another target is not a substitute for them. The named-pipe bridge still requires its separate native acceptance.

Unix socket paths have an operating-system length limit, including the canonical home path. On macOS, `/var` resolves to `/private/var`, which adds bytes. Use a short isolated home such as `/tmp/dim-dev` for development/tests if the daemon reports `path must be shorter than SUN_LEN`; the CLI preserves that failure in `rust/daemon.log`.
