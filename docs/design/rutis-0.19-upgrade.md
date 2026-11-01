# Upgrading to rutis 0.19.3

Status: Ready.

Issue: [#681](https://github.com/arcships/dim/issues/681).

Rules: [design rules](../architecture/rust-agent/principles.md), [ADR 002](../adr/002-plugin-scope.md),
[ADR 006](../adr/006-plugin-contract.md), [How Dim uses rutis](rutis-usage.md).
[中文](../zh/design/rutis-0.19-upgrade.md)

The upgrade moves `rutis`, `rutis-loader`, `rutis-bridge`, the bundled Node runtime, the repository's
TypeScript plugins and the Python SDK from 0.17.1 to 0.19.3. rutis 0.18 and 0.19 both break
compatibility ([0.18 migration](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.18.en.md),
[0.19 migration](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.19.en.md)). The upgrade
keeps no compatibility path. Where rutis removed an API, Dim adopts the form rutis prescribes and adds
no wrapper that rebuilds the old one.

## 1. Scope, and why it is one step

| Item | Now | Target |
| --- | --- | --- |
| `Cargo.toml` workspace dependencies | `rutis`, `rutis-bridge` 0.17.1; `rutis-loader` 0.17.1 with `node`, `python` | 0.19.3; `rutis-loader` with `node` only (0.19 has no `python` feature; `node` enables `runtimes`) |
| New dependency | — | None. Dim starts its runtimes itself (M4); the npm `@arcships/rutis-host` is a test-only development dependency (M10) |
| `xtask` | `rutis-bridge` with `exports` | `rutis-types` 0.19.3 |
| `runtime/package.json` and lock | `@arcships/rutis-runtime`, `@arcships/rutis` 0.17.1 | 0.19.3; plus `@arcships/rutis-host` 0.19.3 as a development dependency (M10) |
| `runtime/plugins/{goal,scheduler,session-title,workflow}/package.json` and locks | `@arcships/rutis` 0.17.1 | 0.19.3 |
| `npm/package.json` peer dependency | `@arcships/rutis` `~0.17.0` | `~0.19.0` |
| `runtime/python-requirements.txt` | `rutis==0.17.1` with its hash | `rutis==0.19.3` with its hash |
| Authoring pin | [authoring guide](../typescript-plugin-authoring.md) | 0.19.3 |
| `.github/workflows/ci.yml` | The Node bindings step runs the plugin tests on 0.17.1's test tool | It exports `RUTIS_HOST` for 0.19's test tool (M10) |

0.19 speaks session protocol 4 and refuses 0.18 and older ends at the handshake, in both directions.
The App Server, the Node runtime, the Python runtime and the interactive `dimcode` process (a UI leaf,
M8) therefore move together. Upgrading only the Rust side leaves every TypeScript and Python row unable
to connect.

The target is 0.19, not 0.18. Most of 0.18's breaks (diagnostics, configuration checks) carry
into 0.19 unchanged, while the APIs Dim would have adapted to on 0.18 (links, `LocalRuntime`,
`RuntimeResolver`) are removed by 0.19. The pins are 0.19.3. 0.19.1, released on 2026-10-30 on
crates.io, npm and PyPI, adds the synchronous waterfall M2 needs
([rutis#947](https://github.com/arcships/rutis/pull/947)) and the public `mark_scope` that
[#684](https://github.com/arcships/dim/pull/684) builds on
([rutis#948](https://github.com/arcships/rutis/issues/948)). It speaks the same protocol as 0.19.0,
so the `npm` peer range stays `~0.19.0`. 0.19.2 fixes bugs only and adds no API; the upgrade takes
it for the linear close peak of [rutis#978](https://github.com/arcships/rutis/pull/978) (#714).
0.19.3 renames rutis's own two services, `loader` and `sessions`, to `rutis/loader` and
`rutis/sessions` ([rutis#986](https://github.com/arcships/rutis/issues/986)), so that a host keeps
both names for its own services, and returns the cleanup errors of a construction that a dispose or
a subtree shutdown cancels ([rutis#983](https://github.com/arcships/rutis/issues/983)); the upgrade
takes it for both.

Not in scope: Go, Swift and Kotlin runtimes, dylib plugins, remote runtimes and node connection rows
(`rutis-host/dial`, `rutis-host/listen`), the `rutis-host` command, and Cordis hosts. Dim uses none
of them. `rutis-migrate` stops at 0.18 and is not used.

## 2. What changed between 0.17.1 and 0.19.3

| Version | What matters to Dim |
| --- | --- |
| 0.17.2 | No API change. A bad value fails only its call; a runtime exit blames no other row; `StillWaiting` after 10 s |
| 0.18 | Error texts carry codes (`session::Error::Binding`); the default error sink writes to standard error; configurations are validated against their schema and filled with defaults; `reconcile` no longer waits for a runtime still starting; objects with methods cross to every language |
| 0.19 | A local runtime is the far end of a session; the link, peer, transport and identity layer is gone; the catalog shares every unregistered name; runtime rows honour `isolate`; every runtime emit goes through the host; `LoaderPlugin` provides the service `loader`; one `rutis/row` listener per loader instead of one per row ([rutis#808](https://github.com/arcships/rutis/pull/808), the fix [#518](https://github.com/arcships/dim/issues/518) waits for); the SDK test tools run on a real `rutis-host`; Python 3.12 or later; `CordisError` is `Error` on the wire |
| 0.19.1 | The synchronous waterfall is back as `waterfall(event).sync(terminal)` (M2); foreign definition copies resolve in a host's own instances, and `mark_scope` is public (rutis#948, used by #684); `Loader::instance_rows`; a `reconcile` after a runtime's `ready()` loads its rows; rutis-host's runtime plugin is Starting from when it is made (rutis#964; Dim does not use it, M4) |
| 0.19.2 | Bug fixes, no API: the close peak of many instances is linear (rutis#978); a loading plugin the driver cancels returns to Pending, and its cleanup errors go to the error sink only (rutis#969, see §8); an abandoned `reconcile` no longer holds later operations; the TypeScript test tool unloads at the end of each test |
| 0.19.3 | The breaking rename of rutis's own services: `loader` becomes `rutis/loader`, `sessions` becomes `rutis/sessions`, and only code that injects or uses them changes (rutis#986); a construction a dispose or a subtree shutdown cancels unloads to `Disposed` and that dispose or shutdown returns its cleanup errors again (rutis#983); the TypeScript test tool returns when a callback and a fake call interleave (rutis#987), and a failed fake provide ends only its `load` (rutis#988) |

### 2.1 Kernel APIs Dim calls

| 0.17.1 | 0.19.3 |
| --- | --- |
| `Ctx::waterfall_sync(event, terminal)` | Removed in 0.19.0, which names `waterfall` (asynchronous) or the synchronous `bail`; neither fits the `before` chain. 0.19.1 adds `ctx.waterfall(event).sync(terminal)` (rutis#946), which M2 uses |
| `SyncCall::next()` in a synchronous listener | Always `Err(WaterfallNext)`: a synchronous listener has no rest of the chain to run |
| Default error sink of `Ctx::root()` | Still writes every reported kind to standard error, without the `[rutis]` prefix; 0.18 reports more kinds: `StartFailed`, `RuntimeDown`, `StillWaiting` |
| `ErrorKind` | Gains `StartFailed`, `RuntimeDown`, `NotServedByName`, `StillWaiting`; `DisposeWaitError::TimedOut` gains `stuck` |

`Ctx`, `DynPlugin`, effects, `Key`, `TypeKey`, `Ctx::isolate`, listeners, `on_sync`, `prepend` and
`#[rutis::service]` keep their 0.17.1 contracts.

### 2.2 Bridge and loader APIs Dim calls

| 0.17.1 | 0.19.3 |
| --- | --- |
| `rutis_bridge::runtime::LocalRuntime` (`node`, `python`, `named`, `interpreter`, `python_path`, stdio, `handle`) | A connection row of the host's own: `rutis_bridge::runtime::Launcher`, `pipe::greeted`, `Session::new`, `pipe::Processes`. The guide's `rutis_host::connect::SpawnPlugin` is not used (M4) |
| `RuntimePlugin`, the service `runtime::Runtime` | Removed; the connection plugin keeps its runtime's `RuntimeStatus` (M4) |
| `Stdio::Lines` | `pipe::Stdio::Pipe(factory)`; Dim's factory forwards lines to `tracing` |
| `rutis_loader::runtime::RuntimeResolver::node` / `::modules`, `with_catalog`, `invalidate` | One `rutis_loader::runtime::SessionResolver` (`npm(&catalog, row)`, `attach(&loader)`, `follow_row(row)`); no cache to invalidate; Node resolves npm names |
| `RuntimeRows`, `RowsSource`, `RuntimeRowsPlugin` | `SessionRows::key(row)`; `SessionResolver::follow_row` |
| `ServiceCatalog::share_registered_only()` | Removed: every name not registered otherwise is shared by name |
| `Loader::waiting_for(entry)` | `Loader::waits(entry)` |
| `Loader::set_overlay`, `overlays()` | Removed: a layer of one's own in the list given to `reconcile` |
| `PeerPlugin`, `LinkConfig`, `LinkState`, `Features`, `Peer`, `peer_key`, `plugins.load` / `plugins.unload` | `Session::handshake(&ctx, channel)`, then mount the `Greeted`, whose `id()` names the far end; running a plugin there is `SessionHandle::run` or a `<row>:<plugin>` loader row, which Dim does not use (M8) |
| `Identity`, `StaticIdentity`, `IdentityPlugin`, `identity_key` | `rutis_bridge::LocalId(PeerId)`, one per kernel |
| `Transport`, `Dial`, `transport_key` | Removed: a connection is a `Channel` handed to a session |
| `transport::local::framed` | `pipe::framed` |
| `Closer::close(&str)` | `Closer::close(Close)` |
| `StaticCatalog::with(name, Described { .. }, factory)` | `StaticCatalog::with(name, Installed::new(factory))`, provided at `catalog_key()`; not used: the leaf offers no catalog (M8) |
| `rutis_bridge::build::Exports` | `rutis_types::Exports`, same API; the generated header changes |
| `LoaderPlugin` provides `Loader` and the mounter | Also the marked service `rutis/loader` and the session catalog at `catalog_key()` |

`host_key`, `HostDispatch`, `Build`, `ServiceCatalog`, `Builtins`, `Resolved`, `mounter_key`,
`session::Error` and `settle` keep their contracts.

### 2.3 Behaviour changes that touch Dim's correctness

- **Standard error.** The default error sink wrote what it reported to standard error on 0.17.1
  too. 0.18 reports more: a start failure nobody awaits, a runtime going down, a plugin still
  waiting after 10 s. The loader's configuration warnings go there as well. The interactive
  `dimcode` process holds the terminal while the TUI runs (M7).
- **Configuration is checked** against each row's schema and filled with defaults before `build`
  (0.18). A missing configuration of an object schema becomes `{}`, so Dim's modules (`NoConfig`) pass.
  TypeScript plugins whose configuration does not match their schema now fail their row.
- **`reconcile` does not wait for a runtime still starting** (0.18). Its rows wait for the runtime and
  are not in the report.
- **npm names are resolved by the Node runtime** (0.19). An unknown bare name waits for the runtime's
  session instead of failing at once.
- **Every runtime emit goes through the host** (0.19). Dim's plugins only listen to events Rust emits.
- **Runtime rows honour `isolate`** (0.19). Dim writes no `isolate` on a runtime row; Session and branch
  TypeScript parts are definition copies without one.
- **A session exports and announces.** Every session exports every process-wide shared service that
  a non-platform plugin provides, and announces every listener name, to its far end. That includes
  the Node and Python runtimes' sessions as well as a UI leaf's. The far end may run any plugin the
  near end's session catalog resolves (0.19).
  - Injection still gates a runtime row's start.
  - Methods tied to the calling row refuse a caller that did not declare the name (`not-declared`,
    rutis-bridge `marked.rs:842`).
  - Dim relies on neither for isolation: installed plugins are trusted code (rule 11).
- **Error texts change and carry codes** (0.18). Dim matches no rutis error text. Its hand-written
  dispatchers keep returning `session::Error::Value`.
- **SDK test tools run on `rutis-host`**, and the plugin under test must be its module's default
  export (0.19).
- **Python 3.12 or later** (0.19). The downloaded runtime is 3.12.15 and CI uses 3.12.

## 3. Dim's current usage

A trial build of all targets (`cargo check --workspace --all-targets --keep-going`) at 76a5105f
against the released 0.19.1 gives the table below. A second run with the two `dim-agent-runtime`
errors fixed as M2 and M3 say reached the crates behind it. Crates neither run reached are located by
search. The same build at 70ad3835 against 0.19.0 gave the same table. The implementation re-runs
the build on its own base before it starts.

| Crate | Result |
| --- | --- |
| `dim-agent-runtime` | 2 errors: `waterfall_sync` (`core/timeline.rs:318`), `share_registered_only` (`names.rs:95`) |
| `dim-agent-types`, `dim-agent-capabilities`, `dim-agent-aimux`, `dim-agent-attachments`, `dim-agent-questions` and their tests | Build once those two are fixed |
| `dim-app-server` | 16 errors: `lib.rs` (runtime rows, `NodeRuntime`), `session_typescript.rs`, `runtime_manager.rs`, `plugin_manager.rs`, `ui_leaf.rs` |
| `tests/integration` | `extensions.rs` (a `LocalRuntime` and its resolver); every other test builds |
| Not reached, located by search | `dim-cli/src/leaf.rs`; `xtask/src/types.rs`; `dim-app-server/tests/transport_flow/ui_host.rs` and `session_typescript.rs:1674`; `set_overlay` in `transport_flow/{scheduler,workflow,process_typescript}.rs` (9 calls); `examples/session_typescript_spike.rs`; the two `ui.leaf` feature strings M8 renames (`dim-app-server/src/transport.rs:725`, `dim-cli/src/lib.rs:148`) |
| Agent placement (M6, not a rutis error) | 120 `build()` and 39 `build_in` calls in 41 files: tests, examples, and `xtask/src/main.rs` (`graph`, `measure`) and `xtask/src/plugins.rs` (the local graph) |

**Open work that meets the upgrade.**

- [#518](https://github.com/arcships/dim/issues/518) waits for rutis#808, which 0.19 ships. The
  upgrade closes it on the measurement of §6.
- [#645](https://github.com/arcships/dim/pull/645) serves [#594](https://github.com/arcships/dim/issues/594)
  with `Loader::instance_rows`, which 0.19.1 publishes (rutis#941) after rutis#920 was closed
  unmerged. It landed on 0.19.2 as a follow-up: the assembly reads its current row handles through
  that API and no behavior changes. That is not how #594 goes: Sessions and branches become plugins
  that mark their scope with the public `mark_scope`, so no instance rows are left to query.
  [#684](https://github.com/arcships/dim/pull/684) carries that design (landed, 79fbb1b3), and its
  implementation follows this upgrade.

## 4. Decisions

### M1. One upgrade unit for host, runtimes, leaf and plugins

Bump every pin of §1 in one commit. The handshake refuses a mixed set, so no intermediate state builds
a working product. `runtime/plugin-sdk.test.mjs` packs `npm/` and type-checks the authoring guide, so
the authoring types and the SDK version move together.

### M2. The Timeline's `before` chain runs on the synchronous waterfall

0.19.0 removes `Ctx::waterfall_sync`, and a synchronous listener can no longer call the rest of its
chain. Neither replacement the 0.19 migration guide names fits the chain:

- **The asynchronous `waterfall`** would make `Timeline::append` and every caller asynchronous, and an
  `on_sync` listener cannot call `next` in it (`NextMisuse::SyncInAsync`). `Timeline::commit` runs the
  chain under the append guard and the commit lock, where it allocates sequences.
- **The synchronous `bail`** has no `next`, so it cannot compose middleware.

[rutis#946](https://github.com/arcships/rutis/issues/946) restores the synchronous waterfall as
`ctx.waterfall(event).sync(terminal)`. It was merged in
[rutis#947](https://github.com/arcships/rutis/pull/947) and released in 0.19.1. The `before` chain
uses it:

- `commit` calls `ctx.waterfall(offered).sync(|offered| Ok(Some(offered.draft.clone())))` where it
  calls `waterfall_sync` today. The transforms keep `on_sync` and call `next` as they do today.
- These are unchanged:
  - the private name `dim/timeline/{instance}/before` and `Timeline::before`
    ([0.16 U2](rutis-0.16-upgrade.md#u2-branch-mechanism-channels-get-private-names));
  - the order: middleware registered earlier wraps later ones, and later registrations transform
    first;
  - the failure rule: a failing transform leaves the original draft and no steps.

No product code registers a `before` transform today; only tests do.

### M3. The catalog shares every unregistered name

Delete `share_registered_only`. Names registered in the catalog keep their keys. A name a foreign row
injects that the catalog does not register now waits in rutis at its shared key, instead of being left
to the runtime. [How Dim uses rutis §3.4](rutis-usage.md#34-the-catalog) drops the sentence about
`share_registered_only`.

### M4. A local runtime is a connection row

0.19 makes a local runtime the far end of a session and removes the runtime plugin and its state
(`LocalRuntime`, `RuntimePlugin`, the `runtime::Runtime` service). The migration guide names
rutis-host's `SpawnPlugin` as `LocalRuntime`'s replacement. That plugin serves rutis-host's own
commands: it keeps `RuntimeStates` for rutis-host's `status` and `restart`, and reads the runtime's
process through `SessionHandle::process()`, which rutis-bridge hides (`#[doc(hidden)]`; it is for
rutis-loader). Dim is a host itself and needs neither. In 0.19 a runtime, local or remote, is a
connection row: a connection of one's own is a plugin that hands a `Channel` to `Session::new`, and a
runtime its host starts takes `<channel> --id <name> <project>`
([0.19 migration](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.19.en.md) "Upgrade
together"). The plugin that starts the process is the one that knows whether it runs, so it keeps
that state itself. Dim links no `rutis-host` and copies none of `SpawnPlugin`.

- **The runtime is a connection row.** `session-node` and `py` are process rows. Their plugin,
  `RuntimeConnection`, does what any connection plugin does, with rutis-bridge's public parts:
  - it starts the process with `rutis_bridge::runtime::Launcher` from the runtime's launch command,
    on a channel of its own with `--id <name>`, records it in a `pipe::Processes` of its own, and
    waits for its greeting (`pipe::greeted`);
  - it mounts `Session::new(channel)`. The loader marks the row, so the session's handle is at
    `session_key(row)`;
  - it keeps its runtime's status (below), and a task of its own watches the session's end;
  - unloading kills a process that has not ended and waits for it (`Processes::kill`,
    `Processes::ended`);
  - it forwards standard error line by line to `tracing` through `pipe::Stdio::Pipe`. Standard input
    and output are null, as today.
- **The far end's plugins are rows.** A TypeScript or Python plugin is the row
  `session-node:<plugin>` or `py:<plugin>`. `SessionResolver` resolves it through that session, and
  it waits for `SessionRows::key(row)` until the session is there. When the session ends, its rows wait
  again. The loader holds all of them; Dim keeps no list of its own.
- **The connection keeps its runtime's status.** A row's own state cannot say that a runtime died: a
  row is Active once its `apply` returns, and nothing moves an Active row to Failed. rutis's own
  connection rows keep the row Active and report the connection beside it (`rutis-host/dial`'s link
  state, `SpawnPlugin`'s `RuntimeStates`). `RuntimeConnection` does the same with a small value of
  Dim's own, `RuntimeStatus`: `starting`, `ready`, or `down` with a reason.
  - It provides the value from the start of its `apply` at a Rust-only process key named after its
    row (`Key::<RuntimeStatus>::dynamic(row)`), as `starting`.
  - It sets `ready` once the session is Active.
  - It sets `down` when the launch or the greeting fails, with that error, and when the session ends
    on its own, with how the process ended (`Processes::status()`, for example `exited with exit
    status: 1`). The task that watches the session's end sets it. The row stays Active, as rutis's
    connection rows do, so the status stays readable; the runtime's rows wait again.
  - `RuntimeStatus` is not rutis's `RuntimeStates`, whose `ready` takes the process that only the
    hidden `SessionHandle::process()` gives.

  `runtime.list` keeps its statuses. `off`, `acquiring` and `failed` are RuntimeManager's own, for
  acquiring the runtime. Once the launch command is provided, RuntimeManager reads the connection's
  `RuntimeStatus`: `starting`, `ready`, or `down` with its reason; while the row has not started, and
  the key is absent, it is `starting`. `idle` goes. Nothing in Dim holds a `RuntimeStates` or a
  `RuntimeHandle`.
- **Launch commands.** Dim keeps the two commands rutis-host's own launchers use for these packages
  (`languages/node.rs`, `languages/python.rs`), with the interpreters and paths Dim configures today:
  - Node: `node --import tsx src/runner.mjs` in the `@arcships/rutis-runtime` package;
  - Python: `python -m rutis` in its project, with the project and the plugin packages on
    `PYTHONPATH`, and `PYTHONUNBUFFERED=1` and `PYTHONDONTWRITEBYTECODE=1`.
- **When a runtime starts and restarts.** The row injects a Rust-only process service carrying its
  launch command (`nodeRuntime`, `pythonRuntime`), so rutis holds it until the command is there.
  - With configured paths (`--node`, `--js-runtime`, `--python`), the App Server provides the command
    at startup, and the row is required, as today.
  - Under RuntimeManager, RuntimeManager provides it once it has acquired the runtime, instead of
    mounting a `LocalRuntime` as its child. The row is optional, so startup never waits for a
    download.
  - `runtime.install` of a runtime that is `down` drops the command's `Disposer` and provides the
    command again. rutis restarts a row whose injected service is withdrawn and provided again, so
    the row starts a new process. RuntimeManager mounts no children and holds no handle.
- **Identity.** The App Server root provides `LocalId("dim-daemon")`. It is the id this kernel greets
  every session with, the runtimes' and the leaves' (M8). It does not mount `Session::service()`:
  that serves the name `sessions`, which is Dim's `AgentDirectory`.
- **Resolving.** One `SessionResolver::npm(catalog, "session-node")` serves both runtimes, last in
  the chain.
  - `attach(&loader)` is called before the loader mounts, and `follow_row` for each runtime row once
    the loader runs, so a runtime's rows are released as soon as its session is there.
  - `runtime(handle)` is not called. It only lists a `RuntimeHandle` under
    `Loader::status().runtimes`; RuntimeManager reads the connection's `RuntimeStatus` instead.
  - Dim's own wrapper is renamed (`SnapshotResolver`). It keeps checking each row's `provides`
    snapshot and adding `workflowFiles`, and it no longer routes `py:` names.
  - A runtime that is not running is recognised the way the loader recognises it: no schema, and a
    `meta.schema` text. The wording of that text is not matched.
- **Gate.** Admission waits for `SessionRows::key(name)` where it waited for `RuntimeRows`. These go:
  the `NodeRows` part; the `SessionNodeRows` / `SessionPythonRows` rows; and the
  `SessionNodeRuntime` / `SessionPythonRuntime` rows, which the `session-node` and `py` rows replace.
- **Reloading.** `plugin.reload` calls `Loader::reload(row)` only. There is no resolver cache, and the
  Node runtime describes the module again at every resolution.
- **Python.** The system interpreter check requires 3.12.

### M5. `rutis/loader` reaches foreign rows; rows stay PluginManager's record

0.19.3's `LoaderPlugin` serves the marked service `rutis/loader` (`create`, `reload`, `remove`,
`status`) at `host_key("rutis/loader")`. The loader registers the name in the catalog Dim gives it,
so a foreign row injects `rutis/loader` and gets rutis's row service; Dim registers no such name of
its own. rutis's own `tests/loader_service.rs` has a Python row do this.

- Dim configures no editable layer: both of its reconciles pass none. `create` and `remove` are
  refused ("this host keeps no editable configuration, so rows cannot be added or removed"). `reload`
  and `status` work, and a reload changes no layer.
- Dim accepts this under rule 11: installed plugins are trusted code, and nothing they can do through
  `rutis/loader` changes configuration. PluginManager stays the only writer of rows, from
  `plugins.json` ([PluginManager](plugins/PluginManager.md)).
- `LoaderPlugin` is a platform plugin, so no session exports `rutis/loader` to its far end. No Dim row
  provides a service named `rutis/loader`.

### M6. A loader is per host, and an Agent is built in one

A host has one loader. The App Server's loader is its process's, and the App Server places its Agents
in it (`dim-app-server/src/agents.rs:1699`). Two other placements break the rule:

- `build()` gives an Agent a private root and a loader of its own.
- `build_in(ctx)` gives each Agent a loader of its own under a host, and isolates that loader's
  services per Agent so that several Agents can share the host
  ([single-tree](../architecture/rust-agent/single-tree.md),
  [How Dim uses rutis §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts) "Isolation labels").

On 0.19 `build_in` no longer works. `LoaderPlugin` also provides `LoaderService`, which is private to
rutis-loader, so its key cannot be isolated, and a second loader in one host fails with
`ServiceExists`. The upgrade restores the rule, and the Agent keeps one placement:

- **One API.** `AgentBuilder::build(loader, anchor)` is today's `build_at` under the name `build`. The
  host owns the loader, the Agent instance and the root; the Agent creates neither a root nor a
  loader. The App Server changes only the name.
- **Removed.**
  - The loader-creating `build()`, `build_in`, `AgentHost::Private` and `AgentHost::Within`.
  - `own_placement`'s per-Agent isolation of `Loader`, `PartRegistry` and the mounter, and its label
    counter.
  - `Agent::close` closes the Agent's Sessions. The host unloads the Agent instance and shuts down its
    root.
- **Tests and `xtask` are hosts.** No product code places an Agent any other way. The 159 calls of
  `build()` and `build_in` (§3) are all in tests, examples and `xtask`.
  - A minimal host under `dim-agent-runtime`'s existing `testing` feature assembles one root and its
    loader the way the App Server does: the builder's modules and plugin rows, `agent_row`, and one
    Agent instance. It builds the Agent in that loader with `build`, and it owns the root's lifetime.
    It takes over `own_placement`'s assembly as a host, not as a placement mode of the Agent.
  - Host plugins mounted on the root before the Agent starts become rows through
    `AgentBuilder::plugin`, or values through `process_value` and `session_value`. In tests these are
    `RecoveryHosts`, `AimuxPlugin`, `ConfiguredModelsPlugin` and others. In `xtask` they are
    `AimuxPlugin` and `ConfiguredModelsPlugin` for `--model-config`, and `FileStoragePlugin` and
    `GraphPlatform` for the local graph.
  - `xtask`'s graphs keep their scopes. The plugins that move become rows of the host's loader, so the
    local graph's process subgraph lists them as such (§6).
  - Host plugins that a test mounts or unloads while the Agent runs (`EnableContributor` in
    `tests/integration/tests/tool_binding.rs`) become rows it adds to and removes from the minimal
    host's loader.
  - `agents_of_one_builder_mount_only_their_own_plugins_parts` (`tests/integration/tests/part_registry.rs`)
    uses two minimal hosts, and still checks that each has its own part registry.
- **Documentation.**
  - `AgentBuilder`'s documentation and single-tree name the one placement.
  - How Dim uses rutis §2 says that tests are hosts like the App Server ("Where plugins run"). It drops
    the sentence about two Agents sharing a host giving their loader, part registry and mounter one
    label ("Isolation labels").

Agents sharing a host keep doing so as in the App Server, where `agent_row` already isolates the
process values. M6 does not depend on 0.19, because `build_at` works on 0.17.1. It lands first, as its
own pull request (§5).

### M7. rutis output never reaches the TUI's terminal

- The `dimcode` root is `Ctx::root_with_sink` with a sink that writes nothing.
  - Failures of the CLI's plugins already reach the CLI where it awaits them (`DaemonLink`, the leaf,
    `Tui`).
  - 0.17.1's default sink already wrote every kind it reported, with a `[rutis]` prefix. 0.18 reports
    more: a start failure nobody awaits, a runtime going down, a plugin still waiting after 10 s.
    Any of them would now reach the TUI's terminal.
- The App Server keeps its `tracing` sink. Its loader's configuration warnings go to `tracing`
  through `Loader::on_config_warning`.

### M8. The UI leaf is a plugin whose life is a session

The leaf keeps the purpose of [Tui §3.6](plugins/Tui.md#36-ui-registry-and-ui-leaves). The daemon is
the authority over which interfaces are alive and can close or replace them. Lifecycle and liveness
are rutis facts, there is no UI-specific control protocol, and business traffic stays on protocol v3.

On 0.19 the daemon no longer loads `Tui` into the leaf. Running a plugin on the far end is a remote run
or a loader row. A remote run has to wait until the far end serves rows, and per-`ui.open` loader rows
would need a second owner of the loader's layers. Neither is needed:

- The daemon's `UiLeaf` is a plugin, as today.
- Its life is the session with the leaf.
- The leaf runs its own `Tui`.

```text
daemon root (LocalId dim-daemon)                 dimcode leaf root
  UiHostPlugin                                     LeafNode
    └─ UiLeaf (ctx.plugin, one per ui.open)          ├─ LocalId ui-<id>
         ├─ ctx isolated for catalog_key()           ├─ accept once, pipe::framed
         ├─ dial address ──── one channel ─────────► │   handshake: far id must be dim-daemon
         ├─ handshake: far id must be ui-<id>        ├─ session ── its end disposes the client tree
         └─ session ◄═════════ rutis session ═══════►┘
                                                    └─ Tui (mounted by the leaf, config from ui.open's reply)
                                                         └─ DaemonLink ── protocol v3 ──► Rpc (ui.attach)
```

- **`ui.open`.** The leaf binds its private address and mounts `LeafNode`, which accepts there, before
  it calls `ui.open {sessionId, kind, peer, address}` over its local connection, as today. The handshake
  can then complete while `ui.open` is in flight.
  - The daemon mounts a `UiLeaf`. `UiLeaf` dials the address, greets, and mounts the session.
  - `ui.open` then replies `{uiId, generation, token}`. The token moves from the hosted `Tui`'s load
    config into this reply; the daemon still keeps only its SHA-256 digest.
  - `ui.open` fails only on the link or the handshake: `ui_leaf_failed`, leaving no registration, as
    today. It no longer waits for `Tui` to load, because the leaf's `Tui` is the leaf's own.
- **Leaf.** `LeafNode` provides `LocalId(ui-<id>)` and offers no plugin catalog, so the daemon can run
  nothing on the leaf.
  - It accepts the one connection on its private address, as `Accept` does today (now a plain function
    returning a `Channel`), and greets with `Session::handshake`. A far end whose id is not
    `dim-daemon` is dropped, which closes the channel.
  - It then mounts the session, and mounts `Tui` once `ui.open` replies. `Tui` takes `uiId`, the token
    and `sessionId` from the reply, and everything else from the leaf's own invocation, as the hosted
    `Tui` did.
  - The end of the session disposes the client tree, which restores the terminal through `Tui`'s own
    effect. The process then exits with the TUI's code.
  - The session's end replaces `LinkState::Stopped` on both sides. Mounting the `Greeted` that
    `Session::handshake` returns gives the session's fiber; its end (Disposed or Failed, through its
    `FiberView`) is the fact. No poll or timer observes the link.
  - A `Tui` that fails to load or ends with an error disposes the client tree in the same way. The
    leaf exits nonzero, its session ends, and the daemon removes the registration.
- **Daemon.** `UiLeaf` dials only the private address the leaf names:
  - a `unix:` socket in the native home on Unix;
  - on Windows, a loopback `tcp:` address whose one-time token it sends as the first line, before the
    handshake, as today.

  Ids in the handshake are self-declared, so on loopback TCP that token is what authenticates the
  leaf. `UiLeaf` greets from a context isolated for `catalog_key()`, so the leaf can run nothing on the
  daemon. It drops a far end whose id is not the `peer` of `ui.open`.
- **States.** A leaf is `starting` from `ui.open` until its `Tui` calls `ui.attach {uiId, token}`, and
  `online` after, as today.
  - **Bound.** A leaf that does not attach within `ui.open`'s existing startup bound (`START`, 20 s),
    counted from the reply, is closed as for `ui.close`. No registration stays without an interface.
  - **Replacement.** `ui.open {replace}` closes the old interface when the new leaf attaches, that is,
    once it is `online`. Tui §3.6 promises that the old one closes after the new one is loaded. If the
    new leaf ends or reaches the bound first, the old interface stays and the claim is released, as
    for a failed start today.
  - Claims and generations are unchanged.
- **Closing.** The session is the leaf's lifetime:
  - The user quits: the leaf disposes its client tree. The terminal is restored and the session closes;
    on the daemon, the session's end disposes `UiLeaf` and `UiHost` removes the registration.
  - The daemon closes the leaf (`ui.close`, replacement, daemon shutdown): it sends the advisory
    `ui.shutdown` notice, then disposes `UiLeaf`. The session closes, and the leaf disposes its tree as
    above.
  - A forced daemon exit ends the session as well.
- **Version mismatch.** `hello` advertises `ui.leaf.2` instead of `ui.leaf`, because the link's
  protocol and `ui.open`'s reply changed. Tui §3.6's existing fallback keeps the direct TUI for an old
  CLI with a new daemon, and for a new CLI with an old daemon.
- **What the link carries** (the [Tui §8](plugins/Tui.md#8-security) update):
  - **The rule.** The session exports every process-group name that crosses languages. The
    [generated catalog](../architecture/rust-agent/generated/catalog.md) lists them, and the installed
    foreign process rows add the names they provide.
  - **Today's set.**
    - `auxiliaryModel`, `managementMethods`, `partRegistry`, `pluginFiles`, `sessionMetadata`,
      `sessionPermissions`, `settings`, `workflowFiles`;
    - `sessions`, whose methods `launch`, `input`, `lookup`, `history`, `withdraw`, `stop`, `idle` and
      `delete` act on Sessions;
    - the default plugins' `scheduleJobs` and `workflowRuns`.
  - **What the leaf could therefore do.** It could act on any Session, register management methods,
    and register Session parts whose copies run in the leaf.
  - **Why this is accepted.** The leaf is reached only through a private address that a local
    `ui.open` caller names. That caller already holds a local protocol v3 connection on `daemon.sock`,
    which reaches the same Sessions and management methods. Its `Tui` uses protocol v3 only. The
    trust boundary stays the local user ([ADR 003](../adr/003-approval-scope.md); AGENTS rule 11).
  - **The Tui §8 rewrite.** §8 is rewritten around this set, and its sentence "The leaf link exposes
    no Session or management method" goes.
  - These services are not isolated from the leaf, although rutis's guidance for a service a far end
    should not see is `isolate` on the providing row. They are Dim's shared cross-language services,
    and the repository's own TypeScript plugins inject them. Isolating them would hide them from those
    plugins as well, for two reasons:
    - SessionTitle's Session part injects `sessions`, `settings`, `sessionMetadata` and
      `auxiliaryModel`. It is a definition copy and carries no row `isolate`
      ([How Dim uses rutis §3.5](rutis-usage.md#35-how-each-runtime-resolves-a-name)).
    - A session exports to every far end alike, and the Node runtime is a session too.
  - The token for `ui.attach` travels only in `ui.open`'s reply, on the local connection.
  - Events are one bus, and listener names are announced in both directions. A leaf can emit any
    event name a daemon listener has, not only `ui/*`, and a daemon listener of that name receives it.
    The leaf's code emits none today; what it could emit is bounded as above.
- **Later UI events.** Tui §3.6 "Later" is rewritten: 0.19 has no `export`/`import` gate and no prefix
  subscriptions, so `ui/*` events from a leaf would reach every daemon listener of their names. Their
  contract is unchanged and remains unimplemented: serde payloads carrying `uiId` and `sessionId`, which
  enter only as adopted inputs.
- **Accepted limits.** Tui §10's two limits go: there is no `peer:` row, and no local transport.
  - One limit is accepted instead of raised: the session catalog's key is `#[doc(hidden)]`.
  - It is accepted because rutis-bridge's catalog module documents `catalog_key()` as where a host
    provides its catalog. Dim uses that documented place, and only to isolate the daemon's catalog
    from the leaf.

### M9. Smaller replacements

- `plugin_manager.rs`: `waitingFor` lists the services of `Loader::waits(entry)`. A row waiting for
  its runtime's session names the runtime's row (`session-node` or `py`), which is how `Loader::waits`
  reports it (`WaitReason::OwnRuntime`). The runtime rows themselves are not plugin rows, so their own
  wait for a launch command is not in `plugin.list`; `runtime.list` shows it instead: `acquiring`
  while RuntimeManager downloads the runtime, `failed` if that failed, and `starting` once the command
  is provided and the row loads. A stuck download stays visible there.
- The nine `set_overlay` test calls write their fixture rows in a layer of their own and reconcile with
  it.
- `tests/integration/tests/extensions.rs` keeps its unresolved-module case with a name that does not
  go through npm resolution. A bare npm name now waits for the runtime instead of failing.
- `xtask types` uses `rutis_types::Exports`. `npm/branch-services.d.ts` is regenerated once for the new
  header. `AgentErrorThrown` keeps its name: only an error type named `Error` is renamed.
- Dim's hand-written dispatchers keep `session::Error::Value` for argument and method refusals. Moving
  them to `Binding` codes is not part of the upgrade.

### M10. TypeScript plugin tests run on `rutis-host`

- `runtime/package.json` adds `@arcships/rutis-host` as a development dependency.
  - The SDK looks for it at `RUTIS_HOST`, then in the working directory's `node_modules/.bin`, then
    on `PATH`.
  - CI runs the plugin tests from the repository root, where neither of the last two finds it.
  - So the Node bindings step in `.github/workflows/ci.yml` exports
    `RUTIS_HOST="$PWD/runtime/node_modules/.bin/rutis-host"`.
- SessionTitle's test loads the `part` named export. That part moves to a module of its own, and both
  `index.ts` and the test import its default export. The other plugin tests already load default
  exports.
- `load(plugin, { config, services })` keeps its shape. A value now crosses between the test and the
  plugin by the rules between processes, and an event the test observes arrives after the emitter
  returns. Tests that read a result right after an `emit` wait for it.
- A test drives a function a plugin handed it through the runtime, not by calling it itself: the
  test's own call of such a function is a synchronous session call, and a plugin cannot await a
  service of the test inside one (`SyncWaitCycle`). `runtime/fixtures/plugin-test.ts` is the row
  that calls it from the runtime and asynchronously; the goal and session-title tests load it next
  to the plugin.
- Those two tests wait for the plugin to stop instead of for one turn of their own event loop: they
  count the calls the plugin makes of the test's services, which crosses between processes.
- The goal and session-title plugin tests run in CI again. 0.19.3 fixes the three test-tool problems
  the upgrade parked the four tests for ([rutis#986](https://github.com/arcships/rutis/issues/986),
  [#987](https://github.com/arcships/rutis/issues/987),
  [#988](https://github.com/arcships/rutis/issues/988)); the two suites were then moved to the
  crossing rules above. Session-title's generation-budget test is skipped: the 30 s budget is a
  timer of the runtime the plugin runs in, and a test cannot advance it.
- The scheduler and workflow plugin tests stay off in CI: 0.19.3's TypeScript test tool cannot
  receive a plugin definition, and both register a part before anything else —
  `partRegistry.registerDefinition` fails with "cannot send a plugin definition: the far end does
  not accept plugin definitions (it did not declare the capability plugins)". The capability is
  Rust's, and the test process declares none, so no double of the test's can receive what the plugin
  registers, although rutis's own design has a test double receive a definition and load it
  ([plugin values R10](https://github.com/arcships/rutis/blob/main/docs/design-plugin-values-2026-10-25.en.md)).
  `tsc` still checks the two plugins, and the Node-backed flows of §6 still load them for real.
- zod's `z.toJSONSchema` result also carries `~standard`, which 0.19's `definePlugin` refuses as a
  Standard Schema, so the goal, scheduler and workflow plugins pass it as plain JSON.

## 5. Implementation plan

M6 lands first, as its own pull request on 0.17.1. It changes no rutis use, and its test moves would
otherwise hide the upgrade's own diff. The handshake then allows no partial upgrade, so steps 1–5 land
in one pull request. Each step is its own commit.

| Step | Work | Gate |
| --- | --- | --- |
| 0 (own pull request, on 0.17.1) | M6: one placement API, the minimal host, and the 41 files moved to it | `cargo test --workspace --locked`; `xtask check` and `graph` with the step 0 graph diff of §6; the placement checks of §6 |
| 1 | Pins and lock files (M1), to 0.19.3; the authoring pin | `cargo metadata` resolves; the lock files name 0.19.3 |
| 2 | Kernel and catalog: M2, M3 | The tests of the crates below `dim-app-server` and of `tests/integration` (`extensions.rs` follows in step 3); the Timeline `before` checks of §6 |
| 3 | Runtimes: M4, M5, M9 | Node- and Python-backed flow tests of the App Server and `tests/integration` |
| 4 | Leaf: M7, M8 | `cargo test -p dimcode --test ui_leaf`; `transport_flow ui_host::` |
| 5 | TypeScript packages and tooling: M10, `xtask types`, `RUTIS_HOST` in `.github/workflows/ci.yml` | Plugin tests in `runtime/plugins/*`; `runtime/plugin-sdk.test.mjs`; `xtask check` |
| 6 | Documentation (§7) | `node scripts/adr-check.mjs`; `node --test scripts/*.test.mjs` |

## 6. Verification

The checks `.github/workflows/ci.yml` runs, at the workspace root, in its order:

1. `cargo deny --locked check advisories licenses` (the dependencies job)
2. `cargo build --workspace --locked`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`
3. `cargo run --locked -p xtask -- check`, then `graph`. `check` also fails on a stale
   `npm/branch-services.d.ts`. The committed graphs come from the mock, local and App Server
   assemblies. None of them starts a language runtime, so M4's removed rows never appear in them. The
   expected diff of each pull request:
   - Step 0 (M6):
     - `generated/services.json` and `generated/app-server/` are expected not to move;
     - in `generated/local/`, `FileStorage` and `HostPlatform` move from the root to rows of the
       host's loader;
     - the generated blocks of the plugin pages regenerate with it.
   - The upgrade: only fibers that rutis 0.19 itself adds.

   Each pull request records its measured diff and explains every change against this list.
4. `node --test scripts/*.test.mjs`
5. The actual CLI profiles: `cargo test --locked -p dimcode --test cli_flow -- --ignored`, with
   `DIM_TEST_APP_SERVER` and `DIM_TEST_NPM_WRAPPER`
6. The public Node bindings:
   - `runtime/plugin-sdk.test.mjs`;
   - the goal and session-title plugin tests, the scheduler and workflow `tsc` checks, now with
     `RUTIS_HOST` (M10);
   - the Python environment from `runtime/python-requirements.txt`.
7. The Node- and Python-backed Rust flows, with `DIM_TEST_NODE`, `DIM_TEST_JS_RUNTIME`,
   `DIM_TEST_RUTIS_SDK` and `DIM_TEST_PYTHON`:
   - `cargo test --locked -p dim-app-server --test transport_flow --test mcp_http_flow node_ -- --ignored`;
   - `cargo test --locked -p dim-migration --test migration_flow node_ -- --ignored`;
   - `cargo test --locked -p xtask node_ -- --ignored`.

Flow checks the suite must cover; extend existing tests only where the contract moved:

- **`before` order (M2):** two transforms on one branch apply in today's order and record their steps;
  a failing transform leaves the original draft; another branch's transform does not run.
- **Runtime rows (M4):**
  - the `session-node` and `py` rows wait until their launch command is provided, and start once
    RuntimeManager has acquired the runtime;
  - a TypeScript and a Python row start, and `runtime.list` shows their runtimes `ready`;
  - a runtime whose process exits shows `down` with how it ended, its rows wait again, and
    `runtime.install` starts a new process that runs them;
  - a launch command that cannot start shows `down` with the launch error;
  - admission waits for the runtime's rows;
  - `plugin.reload` picks up a changed module.
- **Snapshot rows (M4):** a row resolved while its runtime is down is accepted against its `provides`
  snapshot.
- **`rutis/loader` (M5):** a TypeScript row that injects `rutis/loader` can `reload` a row and read
  `status`; `create` and `remove` are refused because Dim configures no editable layer.
- **Placement (M6):** the tests moved to the minimal host keep their assertions.
  - `agents_of_one_builder_mount_only_their_own_plugins_parts` changes purpose. With one placement
    API, a registry belongs to a host, and two self-placed Agents in one host no longer exist. It now
    checks that two Agents built from one builder in two minimal hosts mount only their own plugins'
    parts. A contributor row that the test adds
  while a Session runs, and later removes, reaches the Session's tools and leaves them again. Closing an
  Agent leaves its instance to the host, and shutting down the minimal host's root unloads it.
- **Leaf (M8):** the existing `ui_leaf` PTY flow (start, `/new`, `/web`, exit code, replacement
  generations, a killed leaf, `ui.close`, daemon shutdown). New checks:
  - `ui.open`'s reply carries the token, the leaf's `Tui` attaches with it, and a wrong token is
    `ui_token`;
  - a `Tui` that fails to load makes the leaf exit nonzero, and the registration goes;
  - a replacement keeps the old interface until the new leaf attaches, and keeps it when the new
    leaf ends first;
  - a leaf that never attaches is closed after `START`;
  - a leaf's attempt to run a daemon plugin fails with no plugin installed;
  - a far end that greets with another id is refused on each side;
  - `hello` offers `ui.leaf.2`, and a client that does not find it runs the direct TUI.
- **Terminal (M7):** a leaf whose daemon disappears prints nothing to the terminal before the TUI
  restores it.
- **Load (#518, #594):** `cargo run --release --locked -p xtask -- measure` on the upgrade's base and
  on its branch.
  - It records, at 1, 10, 100 and 1000 Sessions, the create and close times and the tasks and live
    allocation after close.
  - #518 closes on this evidence.
  - #594's remaining growth in create time is attributed against the new numbers.

## 7. Documentation to update with the upgrade

- [How Dim uses rutis](rutis-usage.md): §1 and §2 (the "Where plugins run" and "Isolation labels" paragraphs, which describe per-Agent loaders and the label counter M6 removes), §3.4 (M3), §3.5 (runtime
  rows and `isolate`), §5 ("Dim is on rutis 0.17.1"), Appendix A (`LocalId`, the runtime binding) and
  Appendix B (Python 3.12).
- [Tui](plugins/Tui.md): §3.1, §3.6 (diagram, Link, Hosted `Tui` becoming the leaf's own `Tui`, Leaving,
  the `ui.open` reply, the `hello` feature paragraph, Later), §8, §9 and §10 (M8).
- [RuntimeManager](plugins/RuntimeManager.md), §1–§3 and §5 (M4):
  - it provides the launch commands (`nodeRuntime`, `pythonRuntime`) instead of mounting
    `LocalRuntime`, and `runtime.install` provides them again;
  - it reads a runtime's state from the connection's `RuntimeStatus` and holds no handle: §2.4's "whether
    a runtime is running is rutis' state (`RuntimeHandle::state`)" and §3.3's "rutis' runtime state"
    become the connection's `RuntimeStatus`, and §5's `runtime.list` statuses lose `idle`;
  - the `session-node` and `py` rows replace the `Runtime#…` names;
  - the Python requirement;
  - its zh mirror, [RuntimeManager](../zh/design/plugins/RuntimeManager.md), the same.

  No client renders `idle` on its own: `dimcode runtime list` prints each status as it comes
  (`dim-cli/src/lib.rs:690-698`), and the TUI has no runtime view.
- [PluginManager](plugins/PluginManager.md): `waitingFor` and reload (M9, M4), and its rutis 0.17.1
  reference in the requirements section.
- The [design overview](overview.md):
  - its management-plane statements that only process plugins register management methods: a far
    end can reach `managementMethods` too (M8);
  - its description of rutis node links with `export`, `import` and `peer:` rows, which 0.19 removes.
- [Scheduler](plugins/Scheduler.md) and the [ACP gateway implementation](acp-gateway-implementation.md):
  their rutis 0.17.1 references. The [ACP gateway acceptance](acp-gateway-acceptance.md) records a
  run on 0.17.1 and stays as it is.
- [Session](session-typescript.md), [branch](branch-typescript.md) and
  [process](process-typescript.md) TypeScript designs, the [authoring guide](../typescript-plugin-authoring.md),
  [single-tree](../architecture/rust-agent/single-tree.md) (also the Agent's placement, M6), [lifecycle](../architecture/rust-agent/lifecycle.md),
  the [plugin pages index](../architecture/rust-agent/plugins/README.md) and the
  [architecture overview](../architecture/README.md): 0.17 references and the runtime and leaf
  mechanisms.
- The `docs/zh/` counterparts where they exist. The generated pages under
  `docs/architecture/rust-agent/generated/` change only through `xtask graph`.

## 8. Risks

| Risk | Handling |
| --- | --- |
| Versions drift across the host, the runtimes and the leaf (the handshake refuses) | M1 bumps every pin in one commit; `ui.leaf.2` keeps a mismatched CLI and daemon on the direct TUI |
| The daemon's services become reachable from a leaf | M8: the private address only (a Unix socket, or loopback TCP with a one-time token), an isolated session catalog, id checks; Tui §8 states the full set, and why `isolate` does not fit |
| Installed plugins reach exported services and `rutis/loader` | Accepted: they are trusted code (rule 11); `rutis/loader` cannot add or remove rows without an editable layer (M5) |
| A leaf starts its session but its `Tui` never attaches | Closed after `START`; a replacement keeps the old interface until the new one attaches (M8) |
| A host gets a second loader | M6: a loader is per host and an Agent never creates one; the App Server and each minimal host hold one |
| Moving 159 calls changes what a test or a graph checks | Each moved test keeps its assertions; M6 lands alone on 0.17.1, so its diff carries no upgrade change; §6 states its expected graph diff |
| #645 lands | It landed after the upgrade, on the published `Loader::instance_rows` of 0.19.2 and with no behavior change, so it conflicts with neither the pins nor this upgrade; the instance rows it reads go when #684 lands (§3) |
| rutis prints into the TUI's terminal | M7's sink; rutis-bridge's own `eprintln!` when a session cannot run plugins is covered by the terminal check |
| TypeScript plugin configurations that did not match their schema now fail | The plugin tests and the Node-backed flows run with the shipped configurations |
| Unknown npm names wait instead of failing | `RuntimeManager`'s status shows the wait; M9 adjusts the test that relied on the failure |
| The launch commands Dim keeps drift from the runtime packages | They are pinned with those packages (`runtime/package.json`, `runtime/python-requirements.txt`), and the Node- and Python-backed flows start both runtimes |
| A Session plugin's cleanup fails while a close cancels its construction | 0.19.3 returns the cleanup error from the dispose or subtree shutdown that cancelled the construction, and the runtime keeps it in `Agent::close`'s result; the cancellation itself is not reported as a close failure. `lifecycle_review` checks both |
| The scheduler and workflow plugin tests are off in CI | `tsc` and the Node-backed flows still cover the plugins; their doubles cannot receive the plugin definition they register until rutis's test tool takes one (M10) |

Rollback: revert the upgrade change and the pins. No persisted format moves; Timeline, storage and
wire fixtures are untouched.

## 9. Requirements raised to mechanism owners

One, answered: a synchronous waterfall for the Timeline's `before` chain (M2),
[rutis#946](https://github.com/arcships/rutis/issues/946), merged in
[rutis#947](https://github.com/arcships/rutis/pull/947) and released in 0.19.1; the upgrade targets
0.19.3. Every other decision uses rutis as it is. One limit is accepted rather than raised (M8).
`LoaderService`'s key cannot be isolated, which fits a loader being per host (M6).

## 10. Rule check

The upgrade adds no part, event or fact.

| Rule | Answer |
| --- | --- |
| Principles §4 rules 1–3, 5–7, 9 | Unchanged: no command, handover, `before` contract, batch, context path or fact changes; M2 keeps the `before` contract, its order and its failure rule |
| AGENTS rule 4 (state ownership) | PluginManager stays the only writer of rows: without an editable layer, a plugin can reload a row through `rutis/loader` but not add or remove one (M5). A runtime's state is its connection plugin's `RuntimeStatus`, which that plugin writes and RuntimeManager reads; RuntimeManager keeps only the state of acquiring the runtime (M4) |
| §4 rule 8 (infrastructure unchanged by default) | The changes come from the dependency upgrade and close inside the runtime crate, the App Server and the CLI; the one requirement on rutis is M2's, answered by rutis#947 in 0.19.1 (§9) |
| AGENTS rule 11 (trusted plugins, explicit boundaries) | M8 states what the leaf link exposes and keeps the boundary at the local user; no approval or permission changes |
| AGENTS rule 6 (frozen fixtures) | `crates/dim-agent-types/tests/fixtures/` is not touched |
| ADR 002 D1–D5 | No scope placement, wiring, dependency or configuration change: the runtimes stay process parts and the leaf stays the CLI process's. M6 changes the Agent placement API to one `build(loader, anchor)`, following [ADR 004](../adr/004-loader-plugin-management.md)'s "No second loader" |
| ADR 006 D2 | Dim uses rutis's sessions and resolver directly. It starts its runtimes with rutis's documented form for a host's own connection, and adds no wrapper for removed APIs |

## 11. Ablation

- Removed with no replacement: `NodeRows` and its two rows; the resolver cache and `invalidate`;
  `share_registered_only`; `TuiRow`, the daemon running `Tui` on the leaf, the leaf's catalog and the
  daemon's unload bound for it; the leaf's and the daemon's
  `Transport` implementations, `StaticIdentity` and `UiHostPlugin`'s transport and identity;
  the loader-creating `build()`, `build_in`, `AgentHost::Private` and `Within`, and
  `own_placement`'s per-Agent isolation and its label counter (M6).
- `own_placement`'s assembly moves to the minimal host, for tests and `xtask`, the only callers that
  need one. It is a host there, not a placement mode of the Agent (M6).
- `git diff --exit-code -- npm/branch-services.d.ts` is not added to CI: `xtask check` already fails
  on a stale file.
- Dim keeps its own resolver wrapper only for what rutis does not do: the `provides` snapshot check
  and `workflowFiles`.
- No compatibility shim and no host application. A runtime is a connection row, the form rutis
  documents for a host's own connection, built from rutis-bridge's public parts. The only runtime
  state is the connection plugin's own `RuntimeStatus`. The `rutis-host` crate is not linked, and its
  `SpawnPlugin` and `RuntimeStates` are not copied. `RuntimeManager` no longer mounts children: it
  provides the launch command, and rutis's gating starts the row.
- `ui.leaf.2` reuses the existing feature fallback and adds no version check.
