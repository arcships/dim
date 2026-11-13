<div align="center">

<img src="docs/design/assets/dimagent-banner.png" alt="DimAgent logo above a green wave of light" width="760">

# Dim

**Dim is a paradigm for general-purpose agents that grow into the environments they inhabit.**

[![Rust toolchain 1.100.0](https://img.shields.io/badge/Rust-1.100.0-93450a.svg?logo=rust)](rust-toolchain.toml) [![npm alpha version](https://img.shields.io/npm/v/dimcode-alpha/alpha.svg?label=npm%20alpha)](https://www.npmjs.com/package/dimcode-alpha?activeTab=versions) [![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE) [![Join us on Discord](https://img.shields.io/badge/Discord-Join%20us-5865F2?logo=discord&logoColor=white)](https://discord.gg/ds35K3TadP)

<a href="https://trendshift.io/repositories/336256?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-336256" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/336256/daily?language=Rust" alt="arcships/dim on Trendshift" width="250" height="55"></a>

</div>

An event-driven Timeline connects what an agent senses to what it does next. The runtime is built entirely from plugins. A plugin written in Rust, TypeScript or Python can give the agent a new way to act and the vocabulary to understand when to use it.

In the Rust alpha, you can ask Dim to write a TypeScript tool for itself in a session with permission to do so. Once the plugin loads, Dim can call the tool on its next model request, edit the implementation and reload it in the same session. [Plugin authoring inside Dim](runtime/plugins/plugin-authoring/skills/plugin-authoring/SKILL.md)

## Install

Install the **Rust alpha** from `dimcode-alpha` and launch it with `dima`. We plan to ship the Rust implementation through the existing `dimcode` npm package and its `dim` command. Today that package distributes the earlier TypeScript CLI.

With Node.js `^22.19.0 || >=23.11.0` and npm optional dependencies enabled, run this from a repository you want to explore:

```sh
npm install -g dimcode-alpha@alpha
dima
```

Sign in with `/login` or select a configured model with `/models`. Type `ls` in the chat and press Enter. In this session Dim asks to run `ls -la` in `runtime/plugins`. Choosing **Allow once** lets it list the three plugin directories.

<img src="docs/design/assets/cli-ls.gif" alt="A user sends ls in Dim chat, approves one directory listing, and gets goal, plugin-authoring and workflow." width="760">

For a read-only investigation, start `dima --permission read-only` in the [retry example workspace](xtask/scenario-eval/timeline-branches/sibling-relay/workspace) and ask:

> Read src/retry.py to see what happens when every attempt fails. Open a branch to read tests/test_retry.py. Does the existing test cover that case?

Use `/lines` to follow either investigation. To inspect the events that connect them, see [Timeline export](docs/design/appserver/timeline-export.md).

**Safety.** Read-only mode allows workspace reads and refuses agent file writes and shell commands. Process isolation is configured separately. Plugins, hooks and MCP servers can execute code when enabled. Project-supplied entries start disabled until you enable them locally. [Permission controls](docs/design/cli-commands.md#permission-controls) · [Plugin and command trust](docs/adr/003-approval-scope.md)

## How Dim adapts

- **Grow across languages.** A Python plugin can provide a service that a TypeScript or Rust part calls by name. The service contract stays the same when its implementation changes. [How Dim uses rutis](docs/design/rutis-usage.md) · [TypeScript authoring guide](docs/typescript-plugin-authoring.md)
- **Let events drive behavior.** Each branch commits facts to its Timeline in sequence. Subscribed parts act on those facts and record the outcome, so a later part can pick up where the first left off. On restart, a part can recover its state without repeating a past action. [Timeline](docs/architecture/rust-agent/time-bus.md) · [Event flows](docs/architecture/rust-agent/event-driven-flows.md)
- **Discover what the environment means.** A plugin can introduce an action, describe when to use it, and register a guide entry for the signals it sends. Each branch adopts its own tool declarations. An input guide explains the eight channels of model-facing text, including how to distinguish an operator’s instruction from a reading or a tool result. A plugin can also record an event for another part to consume without putting it in the model’s context. [Tool declarations](docs/architecture/rust-agent/plugins/ToolRegistry.md) · [Model-facing text](docs/design/model-prompts.md) · [Event contract](docs/design/plugin-contract.md#51-timeline-facts)
- **Follow two leads as one agent.** A child branch inherits the parent’s history through a specific event. From there each line builds and compacts its own model context. Parent and child share a workspace and can exchange findings while they work. [Branch design](docs/design/timeline-branches.md)

## Clients, tools and model protocols

The Rust harness can be embedded as a library. The App Server hosts it with persistence and workspace tools, and exposes sessions to clients. The terminal CLI is one such client. [Architecture](docs/architecture/README.md) · [Harness foundation](docs/adr/001-harness-foundation.md)

| Entry point | What it connects |
| --- | --- |
| Native CLI and Web | The CLI keeps output in terminal scrollback. Open `/web` for the Web workspace, `/resume` to reconnect to a saved session, or use `dima --prompt TEXT` for a single turn. [CLI contract](crates/dim-cli/README.md) · [Web workspace](docs/design/web-workspace.md) |
| Agent Client Protocol (ACP) | ACP clients such as acpx can launch `dima acp` over stdio. The gateway connects to the local App Server and handles protocol I/O, session projection and reconnection. Model selection and managed login depend on the daemon’s capabilities. [ACP implementation](docs/design/acp-gateway-implementation.md) · [Client acceptance, including Lody](docs/design/acp-gateway-acceptance.md) |
| Model Context Protocol (MCP) | MCP servers supply tools alongside native tools. Servers supplied by an ACP client are scoped to its session and their configuration is discarded afterwards. Older daemons without this capability ignore the list. The gateway supports one workspace root. [Session-scoped MCP](docs/design/plugins/TemporaryMcp.md) · [MCP OAuth](docs/design/appserver/mcp-oauth.md) |
| Model providers | Aimux connects the harness to OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, Gemini GenerateContent and compatible chat endpoints. The optional Dim account plugin supplies managed login. [Model host](crates/dim-agent-aimux/README.md) · [Dim account plugin](crates/dim-oauth/README.md) |

An ACP client can also start a source-built binary with an explicit home and model configuration:

```sh
./target/debug/dimcode acp \
  --home /absolute/path/to/isolated-home \
  --model-config /absolute/path/to/model.json
```

## API-key setup

For Bash, from a source checkout:

```sh
mkdir -p .dev
cp crates/dim-agent-aimux/examples/openai.json .dev/model.json
export OPENAI_API_KEY=your-key
```

In `.dev/model.json`, set the provider endpoint and credential environment-variable name. Set both `defaultModel.modelId` and the corresponding model entry’s `id` to a model your account can use. Start the installed alpha with that configuration:

```sh
dima --model-config "$PWD/.dev/model.json" --permission read-only
```

The alpha keeps its state in `~/.dimcode-alpha` and can coexist with the earlier CLI. Non-alpha Rust builds use `~/.dimcode` by default. Sessions and account files are not automatically migrated. API-key providers work independently of the account plugin. [Alpha package and platform details](npm/README.alpha.md) · [App Server](crates/dim-app-server/README.md)

## Daily development

Install Rust through rustup. Cargo selects the version pinned in `rust-toolchain.toml`. The native API-key build needs no Node or pnpm. Run the following from the repository root. [Build prerequisites](docs/architecture/build.md)

```sh
./dev --model-config "$PWD/.dev/model.json"
```

To use the same configuration on later runs, copy `.env.example` to the ignored `.env.dev` and set `DIMCODE_MODEL_CONFIG` to the absolute JSON path. Leave `DIMCODE_PROVIDER` and `DIMCODE_MODEL` unset to use its default, or set both to select a catalog entry. The launcher rebuilds the native executable, restarts the development daemon and retains sessions under `~/.dimcode-dev`. Use `DIMCODE_HOME` or `--home` for another isolated home and `--cwd` for another workspace. [Development contract](docs/architecture/development.md)

```sh
./dev --prompt "Explain this project"  # run one turn, then detach
./dev --resume SESSION_ID             # reconnect to the saved session
./dev restart                         # rebuild and restart
./dev stop                            # stop without building
./dev web                             # serve the Web workspace until Ctrl+C
./dev check                           # run formatting and compilation checks
```

To build without the launcher:

```sh
cargo build --locked -p dimcode
./target/debug/dimcode --home /absolute/path/to/isolated-home \
  --model-config "$PWD/.dev/model.json"
```

The CLI also accepts redirected stdin for a single turn. Ctrl+C during a turn requests a stop and waits for it to settle. At idle, Ctrl+C or EOF detaches. Empty approval answers deny the action, and a noninteractive run that needs approval exits with a nonzero status. `dimcode --help` and `--version` work without starting a daemon.

### Workspace

| Code | Responsibility |
| --- | --- |
| [Runtime](crates/dim-agent-runtime), [types](crates/dim-agent-types), [capabilities](crates/dim-agent-capabilities) | Agent and session lifecycles, Timelines, event contracts, input, model execution, permissions and branches |
| [App Server](crates/dim-app-server), [client](crates/dim-client), [ACP gateway](crates/dim-acp) | Session hosting, persistence, RPC transport, product events, questions and protocol adapters |
| [dimcode](crates/dimcode), [native CLI](crates/dim-cli), [npm](npm/) | One executable for the CLI and its `app-server` subcommand, plus platform packaging and command aliases |
| [Model host](crates/dim-agent-aimux), [Dim account](crates/dim-oauth) | Model protocols and account-enabled builds. Account OAuth client IDs are supplied at build time |
| [Tests](tests/), [xtask](xtask/) | Extension and integration flows, architecture boundary checks, generated graphs and measurements |

### Verify

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run --locked -p xtask -- check
node --test scripts/*.test.mjs
```

Public source verification uses local HTTP fixtures for model flows. For standalone binaries, `node scripts/build-all.mjs` builds the release targets or accepts a single target such as `--target darwin-arm64`. The resulting `dist/dimcode-<platform>` executable includes the App Server. Web assets can be installed separately or compiled in with the `embedded-web` Cargo feature. Cross-compiling the script’s Linux and Windows targets requires Zig, `cargo-zigbuild` and the corresponding Rust target. GitHub-hosted runners build the draft GitHub Releases. [Build environment](docs/architecture/build.md) · [Release packaging and cross-compilation](docs/release/README.md) · [Release procedure](.agents/skills/dim-release/SKILL.md) · [Documentation index](docs/INDEX.md)

## Third-party components and licenses

Dim’s original source is licensed under [MIT](LICENSE). The distribution retains upstream licenses and copyright notices. [Distribution notices](NOTICE)

| Component | Use and attribution |
| --- | --- |
| rutis | Plugin loading, named services and cross-language bindings. [Dim’s rutis integration](docs/design/rutis-usage.md) |
| Aimux | Vendored model protocol implementations, distributed under MIT. [Pinned source](vendor/aimux/SOURCE.md) · [Local patches](vendor/aimux/PATCHES.md) · [License](vendor/aimux/LICENSE) |
| grok-build and Ratatui | The native terminal uses components from xAI’s grok-build, including its Ratatui-derived inline terminal. The distribution carries the Apache-2.0 license and upstream attributions under `licenses/grok-build/`. [Source and revision](vendor/grok-build/SOURCE.md) · [Notices](vendor/grok-build/NOTICE) |
| Rust, Web and plugin dependencies | Native archives include `licenses/RUST-THIRD-PARTY-NOTICES.txt` and `licenses/aimux/`. The Web workspace ships `THIRD-PARTY-NOTICES.txt`, including the SIL Open Font License for Geist and Geist Mono. Bundled TypeScript plugins retain their npm license files. [Full inventory](NOTICE) |
