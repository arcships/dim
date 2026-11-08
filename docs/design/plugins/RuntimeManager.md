# RuntimeManager

RuntimeManager supplies the Node and Python rutis runtimes that plugin code runs in, and the uv tool that most Python MCP servers are launched with (`uvx <server>`), ([#476](https://github.com/arcships/dim/issues/476)). It is a process plugin of the App Server, one loader row. It acquires the runtimes and the tool that are on (a download of a fixed version into the home, verified against its official checksum, or one already installed on the machine), keeps what it installed in its own file, and provides a runtime's launch command. The runtime's connection row starts the process once the command is there, and plugin rows wait for that runtime's session under rutis' rules. The runtimes it downloaded also serve the processes the App Server starts — an agent `exec`, `!` shell input, a hook command and a stdio MCP server — as a fallback: their bin directories follow the user's own `PATH` (§5).

The release ships the executable, the default configuration file and the default plugins, not the runtimes ([release packaging](../../release/README.md#local-packaging)).

## 0. Implementation boundary

| Part | M1 (2026-11-01) | Later |
| --- | --- | --- |
| Node: download of the fixed version, verification against the official checksum, `@arcships/rutis-runtime` from the pinned lock, start (§3) | yes | |
| Python: download of the fixed version, verification against the official checksum, the `rutis` package from pinned hashes, start (§3) | yes | |
| uv: download of the fixed version, verification against the official checksum, `source: system` on `PATH`; no launch command, packages or connection row (§3) | yes | |
| `source: system` for both runtimes and uv (§2.2) | yes | |
| Source lists with official sources first and mirrors after (§2.1) | yes | |
| `runtime.list`, `runtime.install`, `runtime.configure`, `runtime.watch`, the `/runtime` panel and the `dimcode runtime` forms (§5, §10) | yes | |
| Default plugins and the default configuration file in the release (§6) | yes | |
| Python import through PluginManager's link directory (§4) | the runtime side here; links are PluginManager's ([#3](https://github.com/arcships/dim/issues/3)) | |
| `RuntimeService::installPackages`: a plugin's locked npm dependencies into its own directory (§5, §9 R1) | yes | |
| Removing runtime versions this Dim no longer uses | | yes |

## 1. Structure

```text
App Server startup
  Parts::new: launch commands and the rows' resolver from fixed paths (§2.3)
  rows: RuntimeManager (required), session-node, py (not required), plugin rows
        │
        ▼
RuntimeManager apply: registers `runtime` methods and RuntimeService, then a task per runtime that is on
        │  acquire (§3): installed and matching the fixed versions? else download → verify → unpack → rename
        ▼
ctx.provide_at(nodeRuntime / pythonRuntime, launch command)
        │ rutis gating: the connection row injects it
        ▼
session-node / py: start the process, greet, mount its session, keep RuntimeStatus
        │ the session releases its rows
        ▼
plugin rows session-node:… / py:…: Pending → Active
```

- **RuntimeManager** (process row `RuntimeManager`, required): its `apply` only registers; acquisition runs in its own tasks, so it is Active at once and startup never waits for a download. A runtime it has acquired gets its launch command provided from RuntimeManager's context, so the command is withdrawn, and the runtime process ends, when RuntimeManager unloads. It mounts no children and holds no handle.
- **The connection rows** `session-node` and `py` are the runtimes. Each injects its launch command, a Rust-only process service (`nodeRuntime`, `pythonRuntime`), so rutis starts it once the command is provided. It starts the process, mounts the session on its channel and keeps the runtime's `RuntimeStatus` (`starting`, `ready`, or `down` with a reason) at a Rust-only process key named after the row. A plugin row runs on that session and waits for it as rutis waits for any runtime; `Loader::waits` names the connection row, which is what `plugin.list` shows ([PluginManager §2.5](PluginManager.md#25-runtimes)).
- **Replaceable**: RuntimeManager is a plugin. Another plugin providing the same launch commands replaces it.
- **A tool, not a runtime**: uv is installed for the user's own commands and for the MCP servers they launch with `uvx`. It has no connection row, no launch command, no packages directory and no `RuntimeStatus`; its `ready` comes from its record (§2.4). `Kind::launches_runtime` is the one place the difference lives: acquisition, `provide`, `install` and `runtime.list` branch on it, and only the system check reads uv's own `--version` format.

## 2. Data

### 2.1 Versions and sources: `runtime/runtimes.json`, compiled into Dim

```json
{
  "node": {
    "version": "24.21.0",
    "checksums": "https://nodejs.org/dist/v{version}/SHASUMS256.txt",
    "sources": ["https://nodejs.org/dist/v{version}/{file}", "https://npmmirror.com/mirrors/node/v{version}/{file}"],
    "files": { "darwin-arm64": "node-v24.21.0-darwin-arm64.tar.gz" }
  },
  "python": {
    "version": "3.12.15+20261003",
    "checksums": "https://github.com/astral-sh/python-build-standalone/releases/download/20261003/SHA256SUMS",
    "sources": ["https://github.com/astral-sh/python-build-standalone/releases/download/20261003/{file}",
                "https://registry.npmmirror.com/-/binary/python-build-standalone/20261003/{file}"],
    "files": { "darwin-arm64": "cpython-3.12.15+20261003-aarch64-apple-darwin-install_only.tar.gz" }
  },
  "uv": {
    "version": "0.13.7",
    "checksums": "https://github.com/astral-sh/uv/releases/download/{version}/sha256.sum",
    "sources": ["https://github.com/astral-sh/uv/releases/download/{version}/{file}"],
    "files": { "darwin-arm64": "uv-aarch64-apple-darwin.tar.gz" }
  },
  "npmRegistries": ["https://registry.npmjs.org", "https://registry.npmmirror.com"],
  "pythonIndexes": ["https://pypi.org/simple", "https://pypi.tuna.tsinghua.edu.cn/simple"]
}
```

- Dim fixes the version and one archive per platform (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `windows-x64`, `windows-arm64`). The archive's SHA-256 is read at acquisition from `checksums`, the publisher's own checksum file (nodejs.org's `SHASUMS256.txt`, the python-build-standalone release's `SHA256SUMS`, the uv release's `sha256.sum`), fetched over HTTPS from that fixed address. Dim trusts the official publisher; no environment variable or mirror replaces that address. A network that intercepts it is the user's network. uv's release serves a per-file `<file>.sha256` beside each archive and the combined `sha256.sum`; Dim pins the combined file, whose lines name the file with `*`. Linux takes uv's statically linked musl archive, so it has no glibc dependency.
- The runtime packages are pinned where they already are: `runtime/package.json` and `runtime/package-lock.json` (`@arcships/rutis-runtime`, matching the host's rutis, with npm `integrity` for every package), and `runtime/python-requirements.txt` (`rutis`, with `--hash`). Both are compiled into Dim; changing the host's rutis version changes them in the same commit.
- Sources are tried in order; the first whose file matches the official SHA-256 is used. A mirror only serves bytes, so it adds no trust. The environment puts the user's own source first: `DIMCODE_NODE_MIRROR`, `DIMCODE_PYTHON_MIRROR` and `DIMCODE_UV_MIRROR` (a base URL the file name is appended to), `DIMCODE_NPM_REGISTRY` (already used by the CLI) and `DIMCODE_PYTHON_INDEX`. No public mirror is configured for uv: none was verified to serve the same file at a version-fixed address.

### 2.2 Configuration: the `runtimes` section of the default configuration file

The default configuration file ([PluginManager §2.3](PluginManager.md#23-the-default-configuration-file)) ships with Dim and gains:

```json
"runtimes": {
  "node":   { "source": "download", "auto": false },
  "python": { "source": "download", "auto": false },
  "uv":     { "source": "download", "auto": false }
}
```

- `source`: `download` uses the archive above; `system` uses the interpreter on `PATH` (`node`; `python3`, or `python` on Windows; `uv`), checked when it is acquired: Node 22.19 or later (the `engines` of `@arcships/rutis-runtime`) with its bundled npm (`npm-cli.js` where the official archives and installers put it; a Node without it fails with a reason naming that path), Python 3.12 or later with pip, uv 0.11.0 or later with `uvx` beside it. A check that fails is that runtime's or tool's failure (§3.3).
- `auto`: `true` acquires the runtime or tool at every start. `false` leaves it off until `runtime.install` (§5); once installed, it is recorded (§2.4) and acquired at every start like an `auto` one.
- The shipped file sets all three to `auto: false`, so a first start downloads nothing. A Node plugin such as Workflow needs `runtime.install` (the `/runtime` panel, or `dimcode runtime install node`) before it can run; a Python MCP server that starts with `uvx` needs `dimcode runtime install uv`.
- The user overrides the preset in `<home>/runtimes/config.json`, the same shape, merged per runtime and field over the preset; a client writes it through `runtime.configure` (§5), which keeps that merge. It is in the home, so only the user's local configuration changes it; no project source does.
- The configuration is read when an action applies it, not once at startup: `runtime.install` reads the file and the environment at that moment (§3.3), so a changed `source` or `auto` applies without restarting the process. `runtime.list` reports the runtime in effect, not the file — a changed file shows up when an action applies it.

### 2.3 Layout under `<home>/runtimes/`

| Path | Content |
| --- | --- |
| `node-v24.21.0-<platform>/` | the unpacked Node archive |
| `node-packages-<lock digest>/` | `package.json`, `package-lock.json` and `node_modules/` from `npm ci` |
| `python-3.12.15+20261003-<platform>/` | the unpacked Python archive |
| `python-packages-<requirements digest>/` | the `rutis` package from `pip install --target` |
| `uv-0.13.7-<platform>/` | the unpacked uv archive: `uv` and `uvx` (on Windows `uv.exe`, `uvx.exe` and `uvw.exe`) |
| `runtimes.json` | RuntimeManager's record (§2.4) |
| `config.json` | the user's override (§2.2), optional |
| `*.partial/`, `*.download` | work in progress; removed before each attempt |

Every path is a function of the fixed versions, the configuration and the home, so the App Server builds the launch commands and the rows' resolver from them before reconciliation, as it does with configured paths; the runtime process starts only when RuntimeManager provides the command. With `source: system`, the interpreter path is looked up on `PATH` when the App Server builds the commands.

The launch commands are those rutis-host's own launchers use for these packages: Node runs `node --import tsx src/runner.mjs` in the `@arcships/rutis-runtime` package; Python runs `python -m rutis` in its project, with the `rutis` package and the project on `PYTHONPATH`, and `PYTHONUNBUFFERED=1` and `PYTHONDONTWRITEBYTECODE=1`. Each process takes its channel, `--id <row>` and its anchor as its last arguments. uv has no launch command: the user's own commands and MCP entries run `uv` or `uvx` from its archive directory, which is on the fallback `PATH` (§5).

The Python runtime's project directory is `<home>/plugins/.python/`, PluginManager's (§4). The Node runtime's anchor is the `package.json` of its packages directory.

### 2.4 `runtimes.json`

```json
{ "node":   { "source": "download", "version": "24.21.0", "sha256": "…", "packages": "node-packages-…" },
  "python": { "source": "download", "version": "3.12.15+20261003", "sha256": "…", "packages": "python-packages-…" },
  "uv":     { "source": "download", "version": "0.13.7", "sha256": "…" } }
```

Written by RuntimeManager only, after a runtime and its packages are installed, or after the uv tool is installed. `version` is the fixed version for `download`; for `system` it is the version of the interpreter, which is checked at every start, and `runtime.list` reports the same. `sha256` is the official digest the archive was verified against. An entry whose source, version and packages equal what the fixed versions and configuration give, with its directories present, is installed: the next start uses it without downloading. uv's entry has no `packages`, because the tool has no packages directory. An entry for an `auto: false` runtime means the user installed it. The record is what `runtime.list`'s `path` reflects: a runtime reports its interpreter exactly while its record marks it installed under the plan in effect. Nothing else is stored; whether a runtime is running is its connection's `RuntimeStatus`, and uv, which launches no runtime, is `ready` from its record alone.

## 3. Flows

### 3.1 Startup

1. The App Server reads the fixed versions and the configuration, builds the launch commands from the fixed paths (§2.3), and adds the rows: `RuntimeManager` (required), `session-node` and `py` (not required), and the plugin rows.
2. RuntimeManager's `apply` registers `runtime` with `managementMethods` and provides `RuntimeService`, then starts one task per runtime that is on (`auto`, or recorded in `runtimes.json`).
3. Each task acquires its runtime (§3.2) and provides its launch command. The connection row and then the plugin rows waiting for its session start by themselves. The two runtimes are independent: one failing does not affect the other.

Startup does not wait for any runtime: a first start without a network reaches the user with the plugin rows Pending.

### 3.2 Acquire

For each runtime, in its task:

1. If `runtimes.json` records it as installed (§2.4), go to step 5.
2. `download`: read the archive's SHA-256 from the official checksum file (a file it does not list is not downloaded), then fetch the archive from each source in order into `<home>/runtimes/<directory>.download`, compute SHA-256 while writing, and keep the first that matches. Unpack it only after it matches, into `<directory>.partial/`, refusing entries that leave the directory and links that point outside it. Rename the unpacked directory to its final path, replacing nothing that verified before. `system`: look up and check the interpreter (§2.2).
3. Packages, into `<dir>.partial/` then renamed: Node runs `npm ci --ignore-scripts --omit=dev --no-audit --no-fund --workspaces=false --registry <r>` with the verified (or system) Node's own npm over the pinned `package.json` and lock, trying each registry in order; npm verifies every package against the lock's `integrity`. Python runs `python -m pip install --require-hashes --no-deps --only-binary :all: --target <dir> --index-url <i> -r requirements`, trying each index in order. uv has no packages directory, so this step installs nothing for it.
4. Write `runtimes.json`.
5. Provide the launch command from RuntimeManager's context. A tool that launches no runtime skips this: its record is what makes it `ready`.

Nothing unverified is executed: the interpreter that runs npm or pip is the verified archive (or the one the user chose with `system`), npm installs with scripts off and checks each package against the pinned lock, and pip refuses any file without its pinned hash.

A plugin's dependencies are installed by the same npm routine, at PluginManager's request (§5): the verified (or `system`) Node's npm runs in the plugin's staging directory over that plugin's `package-lock.json` with the same flags and the same registry order. That install writes no `runtimes.json` entry and creates nothing under `<home>/runtimes/`: the directory is the plugin's, and PluginManager renames or deletes it (§9 R1).

### 3.3 Failure and offline

A runtime whose acquisition fails is `failed` with its reason, which lists each source and its error, in `runtime.list`. Nothing was replaced: a partial directory is removed on the next attempt and the previous installed version, if any, stays. Plugin rows needing it stay Pending, waiting for its connection row's session. The next attempt is the next start or `runtime.install` (§5); there is no automatic retry.

A configuration changed while Dim runs applies on `runtime.install`: the call reads `<home>/runtimes/config.json` and the environment again, and when the `source` it gives differs from the one in effect it withdraws that runtime's launch command, disposes the running connection and acquires under the new source, so the connection row starts again with the new interpreter. A tool has no launch command and no connection to withdraw; a changed source only acquires it again. Nothing else restarts. A configuration that does not read fails the call with its error and leaves the runtime as it was. There is no file watcher: a file no action applies changes nothing.

When the runtime process ends on its own, its session ends and the rows on it go back to Pending. The connection row stays Active and its `RuntimeStatus` is `down` with how the process ended (for example `exited with exit status: 1`); a launch or a greeting that fails is `down` with its error. `runtime.list` shows that status and reason. `runtime.install` of a runtime that is `down` withdraws its launch command and provides it again, so rutis starts the connection row again with a new process.

### 3.4 Upgrade and rollback

A Dim version with new fixed versions acquires into new directories (their names carry the version and digests) and rewrites `runtimes.json` only when the new version is installed. Until then, and if it fails, the previous directories are untouched; an older Dim started on the same home finds its own directories and uses them. Versions this Dim no longer uses are not removed in M1.

## 4. Python import of a plugin installed from a directory

The rutis Python runtime is one process; a row `py:<module>` is imported with `importlib.import_module` from the runtime's `sys.path`, which holds its project directory. A plugin directory may be anywhere (a local path is used in place; a Git install is `<home>/plugins/<id>/<commit>+<suffix>`, and an older record may name it without the suffix).

- The Python runtime's project directory is `<home>/plugins/.python/`, which PluginManager owns; the App Server creates it at startup so the runtime's import path exists before the first plugin.
- When a Python plugin is turned on, PluginManager creates there a directory link `<package>` to the plugin directory (`<package>` is the plugin id with `-` replaced by `_`; a junction on Windows), and creates the row `py:<package>.<entry module>`. A new plugin is importable by the running process without a restart.
- Inside a plugin, modules import each other relatively (`from . import helpers`). Reload re-imports the entry module when its file changed, as rutis does for every Python row ([requirement 3](../plugin-management.md)).
- When a client's `plugin.install`, `plugin.enable` or `plugin.update` turns on a Python plugin and Python is off, PluginManager calls `RuntimeService::install("python")` (§5); the same call applies to a Node plugin in those calls and in PluginManager's background re-acquisition of an optional plugin. It never happens at startup, on an owner-on reconcile, on a reload or for a name in its shipped state. When a plugin's dependencies must be installed, `installPackages` turns Node on and waits for it (§9 R1).

## 5. Interfaces

| Interface | Content |
| --- | --- |
| Launch commands | `nodeRuntime`, `pythonRuntime`: Rust-only process services the connection rows `session-node` and `py` inject; provided once the runtime is acquired. uv has none. |
| `RuntimeStatus` | Kept by each connection row at a Rust-only process key named after the row: `starting`, `ready`, or `down` with its reason. The cell lives in the launch command, so every generation that command starts writes the same cell and each load resets it to `starting`; it publishes every state on a watch channel. RuntimeManager reads it and forwards its changes to `runtime.watch` (§10). uv has none. |
| `RuntimeService` (Rust process service) | `list() -> [RuntimeEntry]`; `install(name)`: turns the runtime or tool on and starts its acquisition; does nothing while it is being acquired or is running; after a failed acquisition, acquires again; once its connection is `down`, provides its launch command again. For uv it installs and leaves the tool `ready`; there is no connection to restart. `installPackages(dir, cancel)`: when Node is off it turns Node on and waits for its acquisition, reading RuntimeManager's own state, then installs a plugin's locked npm dependencies into `dir` with the managed (or `system`) Node's own npm — `npm ci --omit=dev --ignore-scripts --no-audit --no-fund --workspaces=false --registry <r>` over `dir/package-lock.json`, trying `DIMCODE_NPM_REGISTRY` and then `npmRegistries` — and returns when npm exits. Errors: `node_unavailable` with the acquisition's failure reason when Node could not be supplied, `npm_unavailable` (npm could not be started), `install_failed {reason}` (npm's message) and `cancelled`; `cancel` kills npm and leaves `dir` to its caller.Read with an optional dependency, so RuntimeManager's reload does not reload the caller. |
| `ToolchainSource` (Rust port, read by HostPlatform) | `toolchains() -> [InstalledToolchain { bin, root }]`: the downloaded runtimes and tools installed under the plan in effect (§2.4), in the fixed order node, then python, then uv, each with the directory holding its executable (`bin`) and its archive directory (`root`). A `system` interpreter contributes nothing: it is the user's own, and the user's `PATH` already reaches it. HostPlatform reads it at each process start, appends the `bin`s to the child's `PATH` after the user's own and any embedding-host directories, and grants the `root`s to a confined process read-only (§7). The App Server's `ToolchainSource` row answers by reading `RuntimeService` through an optional dependency, so RuntimeManager's reload neither reloads HostPlatform nor leaves it holding a withdrawn service. A host without the row starts processes with the `PATH` it would have without any runtime. |
| `runtime.list` (management method) | `[{name, source, auto, version, path?, status, reason?}]`; `auto` is the configuration in effect, so a client can change it without holding a second copy; `path` is the absolute path of the executable this runtime runs with, present exactly while §2.4's record marks it installed under the plan in effect — the same test that skips a download — and omitted, with `reason` saying so, when that path is not valid UTF-8; `status` is `off`, `acquiring`, `failed`, `ready` for a tool that launches no runtime once it is installed, or, once the launch command is provided, the connection's `RuntimeStatus`: `starting` (also while the connection row has not started), `ready`, or `down` with its reason. |
| `runtime.install {name}` (management method, mutating) | `RuntimeService::install`: reads the configuration, applies a changed `source` by restarting the connection row (§3.3), and replies at once with the status; an unreadable configuration is the call's `runtime_configuration` error. |
| `runtime.configure {name, source?, auto?}` (management method, mutating) | Writes `<home>/runtimes/config.json` atomically, merged per runtime and field over the preset (§2.2), then applies as `runtime.install` does; a file it cannot write or read is the call's `runtime_configuration` error. It refuses while that runtime is being acquired, because a plan cannot change under an acquisition (§3.1). |
| `runtime.watch {}` (management method, read-only) | Opens the calling connection's watch and answers `{}`; the call stays open and its `progress` carries the changes (§10): `{seq: 0, kind: "watching"}` first, then `{seq: n, kind: "runtime", name}` per committed change. A second watch from the same connection answers the first `{}` and takes over. A generation stop answers `operation_interrupted`; a disconnect ends the call with no reply. |
| Client | `/runtime` in both terminal modes (the panel, and the typed `/runtime install|source|auto`), plus `dimcode runtime list`, `install <node|python|uv>` and `config NAME [--source …] [--auto …]` ([CLI commands](../../design/cli-commands.md#plugin-runtimes)). |

## 6. Release

The release archive and each npm platform package carry, beside `bin/`:

```text
bin/dimcode[.exe]
default-plugins.json        # PluginManager §2.3, with `runtimes` (§2.2)
plugins/<name>/             # default and optional plugins; an optional one has no node_modules
LICENSE  NOTICE  release-manifest.json
```

`scripts/stage-plugins.mjs` copies `runtime/default-plugins.json`, each default plugin's directory and each optional plugin's directory (all without tests, `tsconfig.json` or `node_modules`), and runs `npm ci --omit=dev --ignore-scripts --no-bin-links`, from its pinned lock, in each default plugin with a `package.json` — never in an optional plugin, whose dependencies are installed into the user's copy at install time (PluginManager §2.3); `stage-release.mjs --plugins DIR` puts the result beside `bin/`. A default plugin resolves `@arcships/rutis` from its own `node_modules`; it must not resolve a Cordis of its own, since rutis-runtime refuses a plugin whose Cordis differs from the runtime's. The plugin entries are TypeScript, also under an npm package's `node_modules`: the Node runtime loads them through tsx, not Node's own type stripping. Finding the file (`<prefix>/default-plugins.json`, the parent of `bin/`, or `DIMCODE_DEFAULT_PLUGINS`) and the App Server's `--defaults` flag are PluginManager's ([#485](https://github.com/arcships/dim/pull/485)); RuntimeManager reads the `runtimes` section of that file. With `--defaults` and without `--node`, the App Server acquires a runtime with RuntimeManager when its `auto` is on or it is recorded as installed; otherwise it behaves as today (explicit `--node`, `--js-runtime` and `--python` remain for development and tests).

## 7. Security

- Every downloaded archive is verified against the SHA-256 its official publisher lists, read from a fixed HTTPS address, before it is unpacked or run; npm and pip packages are verified by the pinned lock and requirements in the repository. Sources and mirrors add no trust.
- The downloaded Python is externally managed (PEP 668). At acquisition RuntimeManager writes an `EXTERNALLY-MANAGED` file into the interpreter's stdlib directory (`lib/python3.<minor>`, `Lib` on Windows), inside the `.partial` directory before it is renamed, and gives an installed download that predates the marker its marker at the next start. Its `[externally-managed]` error names a virtual environment or uv. A `system` interpreter is never touched. RuntimeManager's own `pip install --target` is not blocked: pip skips the PEP 668 check when `--target` is given (pip 26.2.1, `pip/_internal/commands/install.py` lines 376-392).
- Archive entries are unpacked only inside their directory: absolute paths and `..` are refused, and so are links whose target leaves it (on Windows a link inside is unpacked as a copy); link entries of a zip archive are always refused. No setuid, setgid or sticky bit is kept (on Unix permissions are at most `rwxr-xr-x`).
- npm runs with `--ignore-scripts`; pip installs only wheels (`--only-binary :all:`), so no package code runs during installation.
- RuntimeManager writes only under `<home>/runtimes/`, except the `node_modules` its `installPackages` writes inside the plugin directory it is given; that directory belongs to PluginManager, which renames or deletes it. No credentials are used or stored.
- Integrity is established at acquisition. At later starts RuntimeManager trusts its record and the installed directories without hashing them again: they are in the user's home, which only the user's own processes write, and a process that can change them can equally change the configuration or the plugins themselves. Verification guards what comes over the network, not the home.

## 8. Rule check

RuntimeManager is not a Capability: it has no Timeline state, writes no Timeline event and has no Session or branch part.

| Rule | How this document complies |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | RuntimeManager owns its record and directories. PluginManager asks with `install`; plugin rows' state is rutis', and a runtime's `RuntimeStatus` is its connection row's, which RuntimeManager only reads and whose changes it forwards (§10). HostPlatform reads the installed bin and archive directories through `ToolchainSource` at each process start; the runtimes' directories stay RuntimeManager's, and no other part writes them. |
| Principles §4 rule 2 (handover) | Not applicable. |
| Principles §4 rule 3 (before callbacks only) | No intervention before a write. |
| Principles §4 rule 4 (no business meaning in core) | The connection rows are rutis' form for a host's own connection; rutis gating carries no Dim meaning. |
| Principles §4 rules 5, 6 | Not applicable: no Timeline batch and nothing enters model context. |
| Principles §4 rule 7 (public events and services only) | rutis-bridge's `Launcher`, `pipe::greeted`, `pipe::Processes` and `Session`, and rutis-loader's gating and `SessionResolver`, unchanged and unwrapped. |
| Principles §4 rule 8 (requirements to mechanism owners) | §9: none to rutis; one to PluginManager. |
| Principles §4 rule 9 (only facts later decisions use) | `runtimes.json` records only what the next start decides with: what is installed. |
| Principles §1 | A process plugin of the App Server. |
| ADR 002 D1 (scope follows state) | Process: runtimes are per host. |
| ADR 002 D2 (no wiring modules) | No wiring: plugin rows meet the runtime through its connection row's session. |
| ADR 002 D3 (dependencies follow scope) | Process dependencies only (`managementMethods`). |
| ADR 002 D4 (structural vs business configuration) | `runtimes` is product configuration in the default configuration file; the user's override is in the home. |
| ADR 002 D5 (language-neutral contracts) | Connection rows and JSON methods. |

## 9. Requirements raised to mechanism owners

None to rutis: a connection row starts its process only once its launch command is provided, and rows wait for its session under rutis-loader 0.19.3's existing gating.

**R1. PluginManager: Python links, the runtime request, and a plugin's npm packages (M1).**

| Item | Content |
| --- | --- |
| Blocked by | A Python plugin's directory is not on the running Python runtime's import path; a Node plugin turned on while Node is off can only wait; and a plugin that declares npm dependencies has no way to have its lock installed into its own directory, nor a caller a way to await Node's acquisition before running npm. |
| Existing mechanisms considered | Adding each directory to `PYTHONPATH` and restarting the Python runtime per install (restarts every Python row; same-named modules of two plugins collide); PluginManager running npm itself (duplicates this document's registry, verification and failure rules). |
| Generic capability | PluginManager keeps `<home>/plugins/.python/<package>` links and names Python rows `py:<package>.<entry>` (§4), and calls `RuntimeService::install("python")` when a client's install, enable or update turns on a Python plugin while Python is off; the same call applies to Node in those calls and in its background re-acquisition of an optional plugin. It is never called at startup, on an owner-on reconcile, on a reload or for a name in its shipped state. PluginManager calls `RuntimeService::installPackages(dir, cancel)` (§5) once per acquisition: that call turns Node on and waits for its acquisition when Node is off, then installs the plugin's locked npm dependencies into the staging directory before it is renamed into place, and PluginManager deletes the directory if it fails. |
| Effect on sequence, batch, interrupt, reload | None on the Timeline. |
| Owner and issue | PluginManager, [#3](https://github.com/arcships/dim/issues/3). |

## 10. Client change notices

RuntimeManager's records are process state with no Timeline fact, so a client that shows them reads `runtime.list` and otherwise polls. A client that needs changes without polling opens `runtime.watch {}`; the call stays open while the connection watches, and the changes travel as its `progress` ([methods contract A.2](../contracts/session-boundary.md#a2-call-channel)):

| Item | When |
| --- | --- |
| `{seq: 0, kind: "watching"}` | Once, when the watch is registered |
| `{seq: n, kind: "runtime", name}` | After a committed change of that runtime's entry: the acquisition starts or completes, `runtime.configure` writes the configuration, or its connection row's status changes |

- One item per committed change: `install` announces after the acquiring and plan commit and after the completion write, `configure` after its configuration write, and the row's status forwarder after each state the row writes (§1, §5). Nothing is coalesced; the client re-reads `runtime.list` for the named entry on every item, so a lost intermediate state costs nothing.
- `seq` starts at 0 for each watch call and grows by one per item. Progress is best effort: the connection drops what it cannot take, so a gap tells the client to re-read every entry.
- The client's rules: call `runtime.watch`; after `seq: 0` read `runtime.list`; re-read on each item; re-read every entry on a gap; call again on `operation_interrupted`. `service_reloading` is retryable.
- One watch per connection: a second `runtime.watch` from the same connection takes over and the first call answers `{}`, so one change sends one item. A connection that closes ends its watch; nobody gets a reply.
- A watch belongs to the generation that admitted it. When the RuntimeManager generation stops — a reload, a disable, shutdown — the open call answers `operation_interrupted`, and the client calls `runtime.watch` again; the namespace answers `service_reloading` until the new generation serves it. The stopping generation cannot announce the end through a notification: rutis cancels a generation's token before any task of the stopping generation can run, and its callers answer `gone` from then on (A.2).
- `runtime.watch` declares `progress` and no notifications; the item sheet is additive, and a client that does not handle it ignores it.

## Appendix A. Acceptance (M1)

1. `scripts/release.test.mjs` stages a fixture optional plugin beside a default plugin: the optional plugin's `package.json` and `package-lock.json` name one dependency, the staged `plugins/<name>/` holds its files and no `node_modules`, `npm ci` never runs for it, and the default plugin still gets its own `npm ci`. The flow proves the absence with the stub `npm` on `PATH` that appends the directory it ran in to a log and creates `node_modules` there (`scripts/release.test.mjs:54-64`, the script at `:59`): it asserts the log names only the default plugin's staging directory and never the optional plugin's — the same observation the existing `npm ci: none` flow makes at `:122-123`.
