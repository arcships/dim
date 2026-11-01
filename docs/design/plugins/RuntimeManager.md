# RuntimeManager

RuntimeManager supplies the Node and Python rutis runtimes that plugin code runs in ([#476](https://github.com/arcships/dim/issues/476)). It is a process plugin of the App Server, one loader row. It acquires each runtime that is on (a download of a fixed version into the home, verified against its official checksum, or one already installed on the machine), keeps what it installed in its own file, and provides the runtime's launch command. The runtime's connection row starts the process once the command is there, and plugin rows wait for that runtime's session under rutis' rules.

The release ships the executable, the default configuration file and the default plugins, not the runtimes ([release packaging](../../release/README.md#local-packaging)).

## 0. Implementation boundary

| Part | M1 (2026-11-01) | Later |
| --- | --- | --- |
| Node: download of the fixed version, verification against the official checksum, `@arcships/rutis-runtime` from the pinned lock, start (§3) | yes | |
| Python: download of the fixed version, verification against the official checksum, the `rutis` package from pinned hashes, start (§3) | yes | |
| `source: system` for both runtimes (§2.2) | yes | |
| Source lists with official sources first and mirrors after (§2.1) | yes | |
| `runtime.list`, `runtime.install`; `dimcode runtime list/install` (§5) | yes | client notifications of changes |
| Default plugins and the default configuration file in the release (§6) | yes | |
| Python import through PluginManager's link directory (§4) | the runtime side here; links are PluginManager's ([#3](https://github.com/arcships/dim/issues/3)) | |
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
  "npmRegistries": ["https://registry.npmjs.org", "https://registry.npmmirror.com"],
  "pythonIndexes": ["https://pypi.org/simple", "https://pypi.tuna.tsinghua.edu.cn/simple"]
}
```

- Dim fixes the version and one archive per platform (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `windows-x64`, `windows-arm64`). The archive's SHA-256 is read at acquisition from `checksums`, the publisher's own checksum file (nodejs.org's `SHASUMS256.txt`, the python-build-standalone release's `SHA256SUMS`), fetched over HTTPS from that fixed address. Dim trusts the official publisher; no environment variable or mirror replaces that address. A network that intercepts it is the user's network.
- The runtime packages are pinned where they already are: `runtime/package.json` and `runtime/package-lock.json` (`@arcships/rutis-runtime`, matching the host's rutis, with npm `integrity` for every package), and `runtime/python-requirements.txt` (`rutis`, with `--hash`). Both are compiled into Dim; changing the host's rutis version changes them in the same commit.
- Sources are tried in order; the first whose file matches the official SHA-256 is used. A mirror only serves bytes, so it adds no trust. The environment puts the user's own source first: `DIMCODE_NODE_MIRROR` and `DIMCODE_PYTHON_MIRROR` (a base URL the file name is appended to), `DIMCODE_NPM_REGISTRY` (already used by the CLI) and `DIMCODE_PYTHON_INDEX`.

### 2.2 Configuration: the `runtimes` section of the default configuration file

The default configuration file ([PluginManager §2.3](PluginManager.md#23-the-default-configuration-file)) ships with Dim and gains:

```json
"runtimes": {
  "node":   { "source": "download", "auto": true },
  "python": { "source": "download", "auto": false }
}
```

- `source`: `download` uses the archive above; `system` uses the interpreter on `PATH` (`node`; `python3`, or `python` on Windows), checked when it is acquired: Node 22.19 or later (the `engines` of `@arcships/rutis-runtime`) with its bundled npm (`npm-cli.js` where the official archives and installers put it; a Node without it fails with a reason naming that path), Python 3.12 or later with pip. A check that fails is that runtime's failure (§3.3).
- `auto`: `true` acquires the runtime at every start. `false` leaves it off until `runtime.install` (§5); once installed, it is recorded (§2.4) and acquired at every start like an `auto` one.
- The user overrides the preset in `<home>/runtimes/config.json`, the same shape, merged per runtime and field over the preset. It is in the home, so only the user's local configuration changes it; no project source does.
- The configuration is read when an action applies it, not once at startup: `runtime.install` reads the file and the environment at that moment (§3.3), so a changed `source` or `auto` applies without restarting the process. `runtime.list` reports the runtime in effect, not the file — a changed file shows up when an action applies it.

### 2.3 Layout under `<home>/runtimes/`

| Path | Content |
| --- | --- |
| `node-v24.21.0-<platform>/` | the unpacked Node archive |
| `node-packages-<lock digest>/` | `package.json`, `package-lock.json` and `node_modules/` from `npm ci` |
| `python-3.12.15+20261003-<platform>/` | the unpacked Python archive |
| `python-packages-<requirements digest>/` | the `rutis` package from `pip install --target` |
| `runtimes.json` | RuntimeManager's record (§2.4) |
| `config.json` | the user's override (§2.2), optional |
| `*.partial/`, `*.download` | work in progress; removed before each attempt |

Every path is a function of the fixed versions, the configuration and the home, so the App Server builds the launch commands and the rows' resolver from them before reconciliation, as it does with configured paths; the runtime process starts only when RuntimeManager provides the command. With `source: system`, the interpreter path is looked up on `PATH` when the App Server builds the commands.

The launch commands are those rutis-host's own launchers use for these packages: Node runs `node --import tsx src/runner.mjs` in the `@arcships/rutis-runtime` package; Python runs `python -m rutis` in its project, with the `rutis` package and the project on `PYTHONPATH`, and `PYTHONUNBUFFERED=1` and `PYTHONDONTWRITEBYTECODE=1`. Each process takes its channel, `--id <row>` and its anchor as its last arguments.

The Python runtime's project directory is `<home>/plugins/.python/`, PluginManager's (§4). The Node runtime's anchor is the `package.json` of its packages directory.

### 2.4 `runtimes.json`

```json
{ "node":   { "source": "download", "version": "24.21.0", "sha256": "…", "packages": "node-packages-…" },
  "python": { "source": "download", "version": "3.12.15+20261003", "sha256": "…", "packages": "python-packages-…" } }
```

Written by RuntimeManager only, after a runtime and its packages are installed. `version` is the fixed version for `download`; for `system` it is the version of the interpreter, which is checked at every start, and `runtime.list` reports the same. `sha256` is the official digest the archive was verified against. An entry whose source, version and packages equal what the fixed versions and configuration give, with its directories present, is installed: the next start uses it without downloading. An entry for an `auto: false` runtime means the user installed it. Nothing else is stored; whether a runtime is running is its connection's `RuntimeStatus`.

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
3. Packages, into `<dir>.partial/` then renamed: Node runs `npm ci --ignore-scripts --omit=dev --no-audit --no-fund --registry <r>` with the verified (or system) Node's own npm over the pinned `package.json` and lock, trying each registry in order; npm verifies every package against the lock's `integrity`. Python runs `python -m pip install --require-hashes --no-deps --only-binary :all: --target <dir> --index-url <i> -r requirements`, trying each index in order.
4. Write `runtimes.json`.
5. Provide the launch command from RuntimeManager's context.

Nothing unverified is executed: the interpreter that runs npm or pip is the verified archive (or the one the user chose with `system`), npm installs with scripts off and checks each package against the pinned lock, and pip refuses any file without its pinned hash.

### 3.3 Failure and offline

A runtime whose acquisition fails is `failed` with its reason, which lists each source and its error, in `runtime.list`. Nothing was replaced: a partial directory is removed on the next attempt and the previous installed version, if any, stays. Plugin rows needing it stay Pending, waiting for its connection row's session. The next attempt is the next start or `runtime.install` (§5); there is no automatic retry.

A configuration changed while Dim runs applies on `runtime.install`: the call reads `<home>/runtimes/config.json` and the environment again, and when the `source` it gives differs from the one in effect it withdraws that runtime's launch command, disposes the running connection and acquires under the new source, so the connection row starts again with the new interpreter. Nothing else restarts. A configuration that does not read fails the call with its error and leaves the runtime as it was. There is no file watcher: a file no action applies changes nothing.

When the runtime process ends on its own, its session ends and the rows on it go back to Pending. The connection row stays Active and its `RuntimeStatus` is `down` with how the process ended (for example `exited with exit status: 1`); a launch or a greeting that fails is `down` with its error. `runtime.list` shows that status and reason. `runtime.install` of a runtime that is `down` withdraws its launch command and provides it again, so rutis starts the connection row again with a new process.

### 3.4 Upgrade and rollback

A Dim version with new fixed versions acquires into new directories (their names carry the version and digests) and rewrites `runtimes.json` only when the new version is installed. Until then, and if it fails, the previous directories are untouched; an older Dim started on the same home finds its own directories and uses them. Versions this Dim no longer uses are not removed in M1.

## 4. Python import of a plugin installed from a directory

The rutis Python runtime is one process; a row `py:<module>` is imported with `importlib.import_module` from the runtime's `sys.path`, which holds its project directory. A plugin directory may be anywhere (a local path is used in place; a Git install is `<home>/plugins/<id>/<commit>`).

- The Python runtime's project directory is `<home>/plugins/.python/`, which PluginManager owns; the App Server creates it at startup so the runtime's import path exists before the first plugin.
- When a Python plugin is turned on, PluginManager creates there a directory link `<package>` to the plugin directory (`<package>` is the plugin id with `-` replaced by `_`; a junction on Windows), and creates the row `py:<package>.<entry module>`. A new plugin is importable by the running process without a restart.
- Inside a plugin, modules import each other relatively (`from . import helpers`). Reload re-imports the entry module when its file changed, as rutis does for every Python row ([requirement 3](../plugin-management.md)).
- When PluginManager turns on a Python plugin and Python is off, it calls `RuntimeService::install("python")` (§5).

## 5. Interfaces

| Interface | Content |
| --- | --- |
| Launch commands | `nodeRuntime`, `pythonRuntime`: Rust-only process services the connection rows `session-node` and `py` inject; provided once the runtime is acquired. |
| `RuntimeStatus` | Kept by each connection row at a Rust-only process key named after the row: `starting`, `ready`, or `down` with its reason. RuntimeManager reads it. |
| `RuntimeService` (Rust process service) | `list() -> [RuntimeEntry]`; `install(name)`: turns the runtime on and starts its acquisition; does nothing while it is being acquired or is running; after a failed acquisition, acquires again; once its connection is `down`, provides its launch command again. Read with an optional dependency, so RuntimeManager's reload does not reload the caller. |
| `runtime.list` (management method) | `[{name, source, version, status, reason?}]`; `status` is `off`, `acquiring`, `failed`, or, once the launch command is provided, the connection's `RuntimeStatus`: `starting` (also while the connection row has not started), `ready`, or `down` with its reason. |
| `runtime.install {name}` (management method, mutating) | `RuntimeService::install`: reads the configuration, applies a changed `source` by restarting the connection row (§3.3), and replies at once with the status; an unreadable configuration is the call's `runtime_configuration` error. |
| `dimcode runtime list`, `dimcode runtime install <node|python>` | The methods above. |

## 6. Release

The release archive and each npm platform package carry, beside `bin/`:

```text
bin/dimcode[.exe]
default-plugins.json        # PluginManager §2.3, with `runtimes` (§2.2)
plugins/<name>/             # goal, web, session-title, workflow, scheduler with production node_modules; plugin-authoring
LICENSE  NOTICE  release-manifest.json
```

`scripts/stage-plugins.mjs` copies `runtime/default-plugins.json` and each default plugin's directory (without tests, `tsconfig.json` or `node_modules`) and runs `npm ci --omit=dev --ignore-scripts --no-bin-links`, from its pinned lock, in each one with a `package.json`; `stage-release.mjs --plugins DIR` puts the result beside `bin/`. A default plugin resolves `@arcships/rutis` from its own `node_modules`; it must not resolve a Cordis of its own, since rutis-runtime refuses a plugin whose Cordis differs from the runtime's. The plugin entries are TypeScript, also under an npm package's `node_modules`: the Node runtime loads them through tsx, not Node's own type stripping. Finding the file (`<prefix>/default-plugins.json`, the parent of `bin/`, or `DIMCODE_DEFAULT_PLUGINS`) and the App Server's `--defaults` flag are PluginManager's ([#485](https://github.com/arcships/dim/pull/485)); RuntimeManager reads the `runtimes` section of that file. With `--defaults` and without `--node`, the App Server acquires the runtimes with RuntimeManager; otherwise it behaves as today (explicit `--node`, `--js-runtime` and `--python` remain for development and tests).

## 7. Security

- Every downloaded archive is verified against the SHA-256 its official publisher lists, read from a fixed HTTPS address, before it is unpacked or run; npm and pip packages are verified by the pinned lock and requirements in the repository. Sources and mirrors add no trust.
- Archive entries are unpacked only inside their directory: absolute paths and `..` are refused, and so are links whose target leaves it (on Windows a link inside is unpacked as a copy); link entries of a zip archive are always refused. No setuid, setgid or sticky bit is kept (on Unix permissions are at most `rwxr-xr-x`).
- npm runs with `--ignore-scripts`; pip installs only wheels (`--only-binary :all:`), so no package code runs during installation.
- RuntimeManager writes only under `<home>/runtimes/`. No credentials are used or stored.
- Integrity is established at acquisition. At later starts RuntimeManager trusts its record and the installed directories without hashing them again: they are in the user's home, which only the user's own processes write, and a process that can change them can equally change the configuration or the plugins themselves. Verification guards what comes over the network, not the home.

## 8. Rule check

RuntimeManager is not a Capability: it has no Timeline state, writes no Timeline event and has no Session or branch part.

| Rule | How this document complies |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | RuntimeManager owns its record and directories. PluginManager asks with `install`; plugin rows' state is rutis', and a runtime's `RuntimeStatus` is its connection row's, which RuntimeManager only reads. |
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

**R1. PluginManager: Python links and the runtime request (M1).**

| Item | Content |
| --- | --- |
| Blocked by | A Python plugin's directory is not on the running Python runtime's import path. |
| Existing mechanisms considered | Adding each directory to `PYTHONPATH` and restarting the Python runtime per install (restarts every Python row; same-named modules of two plugins collide). |
| Generic capability | PluginManager keeps `<home>/plugins/.python/<package>` links and names Python rows `py:<package>.<entry>` (§4), and calls `RuntimeService::install("python")` when it turns on a Python plugin while Python is off. |
| Effect on sequence, batch, interrupt, reload | None on the Timeline. |
| Owner and issue | PluginManager, [#3](https://github.com/arcships/dim/issues/3). |
