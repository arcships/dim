# Rust CLI

`dimcode` is a thin Rust client for the local App Server. It owns command-line input, scrollback output, approvals and account prompts. The App Server owns model execution, tools, session state and persistence. The npm wrapper exposes the same program as `dim` and `dimcode`.

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

## Project MCP execution approval

Project `.mcp.json` entries require explicit approval before discovery can launch a stdio
process or connect to an HTTP/SSE server. This is separate from per-tool permission prompts.
An unapproved project entry still shadows a same-name user entry. User-scoped MCP configuration
remains trusted local configuration.

Use the same `--home` and model profile as the daemon serving your workspace:

```bash
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp list
# Review .mcp.json and its referenced code or endpoint after obtaining this fingerprint.
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project \
  mcp approve docs FINGERPRINT_FROM_LIST
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp revoke docs
```

`mcp list` prints tab-separated rows: quoted and escaped ID, source, transport, enablement, approval
state and fingerprint. It does not print commands, environment values, headers or credentials.
For example (the actual fingerprint has 64 hexadecimal characters):

```text
"docs"  project  stdio  enabled  required  <fingerprint>
                  |
             review + approve
                  v
"docs"  project  stdio  enabled  approved  <same fingerprint>
                  |
                revoke
                  v
"docs"  project  stdio  enabled  required  <same fingerprint>
```

Pass the ID's actual value, not its displayed escape spelling. List **before** reviewing the file,
then approve that fingerprint: if the effective configuration changed since listing, approval
fails instead of trusting the new content. Do not automatically pipe a listing into approval.
Approval is an explicit mutating command and works without a TTY; it can immediately start
discovery in active sessions. It does not enable an explicitly disabled server.

Grants are private to the selected host home, canonical workspace, server ID and configuration.
They survive restart, but do not travel with a Session or project. Changed executable settings
require a matching grant. Revocation cancels and joins this host's generation work and closes
connections, including those retained by an in-flight model request. It does not undo previous
remote effects. After an interrupted command, run `mcp list` before retrying. Configuration
approval is not a sandbox and does not detect changed script/binary contents or inherited
environment; see the [host trust contract](../dim-app-server/README.md#mcp-configuration-management).

## Scrollback and session recovery

```text
dimcode · model: openai/your-model-id
workspace: /path/to/project
session: 00000000-0000-0000-0000-000000000000

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

Input is one line per turn. Only `y` or `yes` permits a pending tool operation once; empty input or any other answer denies it. Tool status records and approval reasons are displayed without dumping arbitrary tool arguments or tool result bodies. Model and external text are filtered at the terminal boundary to remove control sequences.

This is the `--no-tui` line client. The terminal keeps the scrollback; there is no alternate screen, multiline editor or client-side input queue. Input typed during execution is discarded before the next input or approval prompt so it cannot accidentally approve an operation.

While a turn is running, `Ctrl+C` requests stop and waits for the server's resource and output delivery barrier. A second interrupt does not skip settlement. Once the prompt returns, `Ctrl+C` or EOF exits. The client detaches; it does not delete the session or shut down a shared daemon.

Every session handle is printed; noninteractive handles go to stderr. Restore it explicitly:

```bash
target/debug/dimcode --home /path/to/isolated-home --model-config model.json \
  --resume SESSION_ID
target/debug/dimcode --home /path/to/isolated-home --model-config model.json \
  --resume SESSION_ID --prompt 'Continue'
```

Interactive recovery displays main-branch history. Noninteractive recovery emits only the current output. History uses a fixed server watermark and its real `nextAfter` positions, including filtered records. Live events and progress snapshots are reconciled with already printed durable text. The client waits for an existing turn before accepting a new prompt. It never automatically retries a submission after a transport failure because the server may already have accepted it.

## Terminal UI modes

The interactive terminal UI has two modes over the same session. Scrollback (the default) commits the transcript into the terminal's native scrollback and repaints only the composer region at the bottom. Fullscreen takes the alternate screen, keeps the transcript itself and scrolls it in a viewport above a dock that never moves. A transcript shorter than the viewport sits on the dock and grows upwards, as in Amp:

```text
 ◆ answer row                                                   ▁   scrollbar: last column,
 ◆ answer row (streaming tail scrolls with the transcript)      █   thumb in eighths of a row
                                                                █
 ╭─ · working…  3s ──────────────────────────────────────────╮
 │ › Ask anything...                                          │      dock: queue, composer,
 ╰────────────────────────────────────────────────────────────╯      panels and status stay
 AGENT  ⏵⏵ default  esc interrupt                 provider / model    pinned to the bottom
```

| Fullscreen input | Effect |
| --- | --- |
| Mouse wheel, `PageUp` / `PageDown` | Scroll the transcript; new output does not move a scrolled view. A wheel notch scrolls three rows; a trackpad, once its dense reports are seen, one row per report; a fast streak speeds up to three (notch) or six (trackpad) times that |
| Sending a message | Follow the bottom again |
| Mouse drag | Select transcript text; release copies it (OSC 52, plus `pbcopy` locally on macOS; inside tmux OSC 52 goes through passthrough, which needs `set -g allow-passthrough on`) |
| Click a link | Opens it in the system browser (OSC 8 links, or a bare `http(s)://` URL); over SSH the status row shows the URL instead |
| `Tab` / `Shift+Tab` | Pick older / newer user messages (`Up` / `Down` while one is picked); past the newest the pick ends. `Esc` ends it, `e` puts the message in the composer to edit and send again |
| `/`, `Ctrl+O`, `Ctrl+P` | Open the command palette centred over the dimmed transcript, with its own query row; a draft set aside by `Ctrl+O`/`Ctrl+P` returns when the palette closes |
| `Home` / `End` | With an empty composer: scroll to the top / bottom of the transcript |
| `Esc` | Interrupt the run |
| `Ctrl+C` | Clear the draft, then exit on a second press; a busy session asks for confirmation instead of being interrupted |

Pickers (`/resume`, `/models`, `/connect`, `/approvals`, `/theme`, `/language`, `/settings`) open as centred boxes too; the mouse wheel moves their selection. Approval and question prompts stay under the composer.

```text
 ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓    picked message: the box
 ┃ ┃ Explain this project                                   ┃    takes the blank rows
 ┗━e to edit━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛    around it, nothing moves
 ◆ The project …      ╭─ Command Palette ─────────────────╮
                      │                                    │    palette: own query row,
                      │ > th                               │    transcript dimmed behind,
                      │                                    │    the selected row on an
                      │▓ view  /theme  switch theme ▓▓▓▓▓▓▓│    accent bar
                      │   app  /exit   exit   Ctrl+C Ctrl+C│
                      ╰────────────────────────────────────╯
```

The palette lists what the interface runs, as Amp's does: each row reads category, the action (a `/command` and its description, or an action a key reaches), and at the right edge the key that runs it (`Esc` interrupt, `Ctrl+S` steer, `Home` / `End` scroll, `Tab` pick a message, `Ctrl+C Ctrl+C` exit). An action that cannot run now (no session yet, nothing running, no queued message) is faint and marked `(unavailable)`; fullscreen-only actions are left out of the scrollback palette. Typing ranks the matches: the exact name, a name prefix, an alias prefix, a prefix of a name part (`reload-skills` for `sk`), a name containing the query, a description word starting with it, then three or more letters in order through the name (`stg` for `settings`); a match inside a description word does not count.

`/settings` gathers the interface settings: theme and language (each opens its picker and comes back), the startup form (`tuiMode`), tool details, the fullscreen mouse (`tuiMouseMode`: off hands selection and the wheel to the terminal), notifications (`tuiNotifyOnComplete`, on by default), tool approvals and the model. Switches flip with `Enter`, `←`/`→` or `Space` and are saved to `config.json` at once.

Notifications reach a user who switched windows when the run finishes or waits on a permission or a question (each request once): iTerm2 gets `OSC 9`, kitty `OSC 99`, Ghostty, WezTerm and foot `OSC 777`; tmux and any other terminal get a bell, which tmux forwards and terminals turn into a Dock bounce, a taskbar flash or a tab badge.

`/keys` writes the interface's keys into the transcript; in scrollback it adds how the terminal that owns the history scrolls, searches and copies (tmux copy mode, iTerm2 and Apple Terminal `Cmd` keys, `Shift+PageUp` in Ghostty, WezTerm, kitty and Alacritty, Windows Terminal).

`/language` (`/lang`, `/locale`) picks the interface language: the system default (`LC_ALL`, `LC_MESSAGES`, `LANG`) or one of English, 简体中文, 日本語, 한국어, 繁體中文, Bahasa Indonesia, Deutsch, Français, Español, Português (Brasil) and Português (Portugal). Moving previews, `Enter` saves it as `locale` in `config.json`, `Esc` restores. As in the TS build, English and Simplified Chinese are translated; the other languages read as English for now. Rows already in the transcript keep their language.

The default theme (`System`) is Amp's: every colour is the terminal's own default foreground, a slot of its 16-colour palette (`ansi:N`) or the faint attribute, so it follows the terminal's colour scheme on dark and light backgrounds alike. Yellow is the accent (titles, the selected row, inline code), green the user's messages and successes, cyan headings, red errors; muted text is faint, table rules bright black, and code is highlighted in the terminal's base colours. `Dim Dark` and `Dim Light` take the same structure with Dim's green as the accent. A theme role is a colour spec: `#rrggbb`, `ansi:N`, `default` or `dim`.

`e` cannot rewrite history: the App Server has no rewind or fork, so the edited text is sent as a new message.

Streaming is paced for the screen: the session worker publishes a snapshot one frame after the first change however fast events arrive, and snapshot rows are shared with the worker rather than copied. Each frame reveals an eighth of the text still waiting (a character at least), so a burst reads as a steady flow that catches up within about eight frames. An open code fence commits its finished lines as they arrive, laid out as the closed block will be, so a long block never piles up above the composer; in scrollback, rows committed from the top of the live region stay where they are drawn, so streaming neither erases the region nor moves the composer, and a changed row is rewritten in place before the rest of it is erased.

The transcript is a virtual list. The CLI keeps each committed block once, as its source (Markdown text, tool records), up to 5,000 blocks; fullscreen keeps only every block's height for the current width and renders the rows of the blocks within about a screen of the viewport, dropping them as it moves away. Heights start as estimates (Markdown by its wrapped source lines) and become exact when the viewport first reaches a block, so a settled resize or theme change lays out only the blocks around the view. A scrolled view is held by its top block, so it stays put while heights above it settle and across a resize. The scrollback interface lays out the newest 10,000 rows of the same history when it rewrites the terminal's scrollback.

`/tui` switches modes inside a running session, including during a run. Entering fullscreen erases the scrollback composer and lays the history out on the alternate screen; leaving rewrites the history into the terminal's scrollback. Exiting fullscreen returns to the shell's screen and prints the resume panel there.

```bash
dimcode --fullscreen            # this run only
dimcode --scrollback default    # save the default mode and exit
```

The default is saved as `tuiMode` in `<home>/config.json`.

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

Noninteractive model text goes to stdout. Session handles, tool status and diagnostics go to stderr. Interactive per-tool approval requires a terminal; in noninteractive mode the operation is denied, the turn is stopped and settled, and exit code `3` is returned. Explicit `mcp approve` is a separate management command. Configuration/transport failures do not silently select a mock model or old core.

| Exit code | Meaning |
| --- | --- |
| `0` | Completed, or normal interactive exit |
| `1` | Invalid configuration, incompatible daemon, login or transport error |
| `2` | Model or turn failed |
| `3` | Interactive approval required |
| `130` | Interrupted noninteractive turn or login |

## Focused checks

The public CLI flow tests exercise the real native binary and NDJSON boundary. Unix PTY checks cover default-deny approval, discarded typeahead, stopping while a frame is fragmented, prompt recovery and hidden callback input:

```bash
cargo test --locked -p dim-cli --test cli_flow
cargo clippy --locked -p dim-cli --all-targets -- -D warnings
```

The production-model flow uses the actual App Server, an API-key environment reference and a local OpenAI-compatible HTTP stream fixture. It checks the eight production tools, a real file read reaching the model, and continued input on the same persisted session:

```bash
cargo build --locked -p dim-app-server
DIM_TEST_APP_SERVER="$PWD/target/debug/dim-app-server" \
  cargo test --locked -p dim-cli --test cli_flow \
  configured_api_key_model_uses_production_tools_and_restores_the_same_session \
  -- --ignored
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

The test uses an isolated home and explicitly shuts down only the daemon it started. The external model service is a fixture; it does not spend a real provider's quota. Windows console and named-pipe behavior still require native Windows acceptance; compiling another target is not a substitute for that check.

Unix socket paths have an operating-system length limit, including the canonical home path. On macOS, `/var` resolves to `/private/var`, which adds bytes. Use a short isolated home such as `/tmp/dim-dev` for development/tests if the daemon reports `path must be shorter than SUN_LEN`; the CLI preserves that failure in `rust/daemon.log`.
