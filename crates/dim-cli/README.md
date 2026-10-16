# Rust CLI

`dimcode` is a thin Rust client for the local App Server. It owns command-line input, scrollback output, approvals and account prompts. The App Server owns model execution, tools, session state and persistence. The npm wrapper exposes the same program as `dim` and `dimcode`.

`dimcode tui` opens the interactive client. `dimcode exec -p TEXT` runs one prompt
and exits; redirected stdin supplies the prompt when `-p` is absent.
`dimcode exec resume SESSION_ID -p TEXT` continues a persisted Session. Existing
`--prompt`, piped stdin and `--resume` entries remain available.

`exec --json` writes one public JSON-RPC notification per line for the selected
Session. Session handles and diagnostics go to stderr. The stream preserves
public fields, includes live observations after attach and does not replay stored
history. Approval is denied, the turn is stopped and settled, and exec exits 3.
Ctrl+C waits for settlement and exits 130.

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
> /exit
$
```

`/export` reads the format and treats the remaining path literally, including
spaces. Errors return to the prompt. `/exit` takes no arguments. Slash commands
in exec or redirected input remain literal model input.
See [CLI commands and exports](../../docs/design/cli-commands.md).

Proposed model commands and atomic Agent configuration adoption are specified in
[model configuration](../../docs/design/model-configuration.md); they are not implemented.

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

`mcp list` prints tab-separated rows: quoted and escaped ID, source, transport and enablement.
It does not print commands, environment values, headers or credentials. `mcp enable ID` and
`mcp disable ID` change the entry that `mcp list` shows for the workspace.

```bash
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp list
# Review the entry in .mcp.json and the code or endpoint it runs, then:
dimcode --home /path/to/isolated-home --model-config model.json --cwd /path/to/project mcp enable docs
```

```text
"docs"  project  stdio  disabled
                  |
             mcp enable docs
                  v
"docs"  project  stdio  enabled
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

The terminal keeps the scrollback. There is no alternate-screen interface, terminal UI dependency, multiline editor or client-side input queue. Ordinary text typed during execution is discarded with a message to wait for the prompt; pending typeahead is cleared before an approval prompt so it cannot accidentally approve an operation.

Interactive `!command` runs in the Session's working directory through `shell.run` and records the command and bounded output for the next model request, without starting a turn. `!!command` keeps its output private to this caller. Both also work during model execution. The prefix must be the first character; leading whitespace
keeps the line ordinary model input. An approval prompt owns its answer line. Commands use the daemon's shell/environment and null stdin. Progress streams under one header per run without adding newlines to partial chunks; the final bounded output is displayed again, including truncation and recording failures. Commands are never retried after an RPC or transport error. Piped or `--prompt` text keeps its literal meaning.

While a turn is running, `Ctrl+C` requests stop and waits for the server's resource and output delivery barrier. A second interrupt does not skip settlement. Once the prompt returns, `Ctrl+C` or EOF exits. The client detaches; it does not delete the session or shut down a shared daemon.

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

While waiting for a daemon it has just started, the CLI waits for the Unix socket to become private before connecting. An existing insecure endpoint is rejected without starting a daemon.

Noninteractive model text goes to stdout. Session handles, tool status and diagnostics go to stderr. Interactive per-tool approval requires a terminal; in noninteractive mode the operation is denied, the turn is stopped and settled, and exit code `3` is returned. Configuration/transport failures do not silently select a mock model or old core.

Requests are limited to 16 MiB of encoded JSON, excluding the NDJSON newline.
The CLI checks the complete request before writing any bytes to the daemon;
JSON escaping can make a prompt exceed this limit even when its original text fits.

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
