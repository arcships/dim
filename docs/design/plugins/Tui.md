# Tui

Status: Implemented. Overall design: [overview §4 Plugin catalog](../overview.md#4-plugin-catalog); scope: [ADR 002](../../adr/002-plugin-scope.md) (CLI process, like [AcpGateway](AcpGateway.md)). Phase: P1. Language: Rust (the existing `dim-cli` code).

The terminal interface, the foreground `dimcode web` holder and the daemon connection they share become rutis plugins in a **client tree**: a rutis root that the `dimcode` client process owns, separate from the daemon's tree. The daemon (App Server), its loader rows and its managed plugins are untouched; the client tree depends on the daemon only through the local RPC protocol v3, exactly as the CLI does today. The plugin boundary buys two things: the client-start/daemon-start order becomes a declared dependency instead of code order, and terminal restoration and session shutdown become `Effect`s that rutis runs exactly once in reverse order on every exit path. `/web` no longer unloads the TUI: it toggles the daemon-owned web switch. Everything the user sees (`ui/`, `model/`, `projection`, `wire`) is unchanged.

This document gives the process model (§1), the startup sequences of every entry point (§3), the timing decisions (§4) and the distribution (§7). It does not move the TUI into the daemon: the daemon is a headless process shared by all clients and has no terminal, and [ADR 001](../../adr/001-harness-foundation.md) keeps the library usable without a TUI.

## 1. Structure

```text
dimcode (one executable, two roles)

  client process (per invocation)                 daemon process (one per product home)
  ┌─────────────────────────────────────────┐     ┌──────────────────────────────────────┐
  │ client tree (rutis root, compiled in)   │     │ daemon tree (rutis root + loader)    │
  │                                         │     │                                      │
  │  DaemonLink ─provides `Connection`─┐    │     │  LoaderPlugin                        │
  │     ▲ discover / spawn / hello     │    │ RPC │   ├─ process rows (Rust)             │
  │     │                              │    │ v3  │   ├─ NodeRuntime, NodeRows           │
  │  Tui ◄─────────────────────────────┤    │◄───►│   │    └─ dim-oauth (TypeScript)     │
  │   session thread + render thread   │    │sock │   └─ session / branch factories      │
  │  WebHold ◄─────────────────────────┘    │     │  WebEntry (loopback listener)        │
  │   `web.open`, waits for a signal        │     │  Transport, Rpc, Agents, ...         │
  └─────────────────────────────────────────┘     └──────────────────────────────────────┘
                                                                     ▲
  browser ◄──────────── http://127.0.0.1:<port>  (Web workspace) ────┘
```

| Part | Scope | Depends on | Provides |
| --- | --- | --- | --- |
| `DaemonLink` | client process | none (gated only by configuration: product home, `--server`, `--model-config`) | `Connection`: the slot holding the open protocol v3 `Client` after `hello` (§6); the daemon's feature list and profile travel with it |
| `Tui` | client process | `Connection` | nothing to other plugins; its result (`Leave::Exit(code)` or `Leave::Restart`) goes to the composition root |
| `WebHold` | client process | `Connection` | nothing; holds the local web entry open while `dimcode web` is loaded |
| Composition root (`run_cli`) | client process | — | parses arguments, builds the tree, loads the plugins an action needs, disposes the tree, returns the exit code |

One-shot actions (`exec`, `export`, `login`, `models`, `mcp …`, `daemon stop`) are RPC calls through `Client`; they are plain functions, not plugins, because they own no resource that outlives the call ([ADR 002 D2](../../adr/002-plugin-scope.md#d2-no-wiring-modules): no wiring modules). `help`, `version` and `--fullscreen default` build no tree.

Not part of this design: a loader in the client (no rows, no editable layer, no Node), a dylib or TypeScript TUI, and the daemon's assembly.

## 2. Data and state

| State | Owner | Durable |
| --- | --- | --- |
| Socket path `<home>/rust/daemon.sock` (Unix) or the pipe name derived from the native home (Windows), server link `<home>/rust/dimcode-server` | `DaemonLink` (computed from configuration each start) | No; the socket file is the daemon's |
| Open connection, `hello` reply (instance id, protocol version, features, daemon profile) | `DaemonLink` | No |
| Session id, projection, view snapshots, input history | `Tui` (its session thread), as `SessionHost` holds them today | Input history and terminal mode are the client's own files under the product home, unchanged |
| Web entry token, address and on/off switch | daemon `WebEntry`; `WebHold` only holds the entry and prints them; TUI `/web` toggles the switch | No |
| Exit code / restart | `Tui`, delivered to the composition root | No |

Nothing in the client tree is Timeline state. The daemon owns every Session fact; a client crash loses only its view.

## 3. Flows

### 3.1 `dimcode` (interactive): client start, daemon start, TUI

```text
client process                                      daemon process
──────────────                                      ──────────────
parse args; stdin is a terminal; action Chat/Tui
Ctx::root()  ── client tree
root.plugin(DaemonLink{options})
  apply:
    native home <home>/rust (0700, owned by user)
    try connect daemon.sock
      ├─ ok ──────────────────────────────────────► hello {profile}
      │                                            ◄ Hello {features, daemonProfile}
      └─ missing / refused:
           spawn `<home>/rust/dimcode-server app-server
                  --unix <sock> --no-ws --home <native home>
                  --product-home <home>`
           (setsid; stdin/stdout null; stderr → daemon.log)
                                                    serve(): rutis root, LoaderPlugin,
                                                    reconcile assembly layer
                                                    (process rows, NodeRuntime, NodeRows →
                                                     dim-oauth) within startup_timeout;
                                                    listen on daemon.sock; chmod 0600
           poll every 50 ms, at most 10 s:
             socket present and not awaiting chmod
             → connect → hello ───────────────────► ◄ Hello
             child exited → fail (see daemon.log)
    provide Connection{Client}
    (no effect: dropping the service drops the Client, which detaches)
  ── DaemonLink Active
  bootstrap the Session:
    --resume ID: session.attach
    otherwise:   session.create {cwd, permissionPolicy, model?}
  session.attach
  leaf node (same process): private socket, PeerPlugin host { "Tui" }
  ui.open {sessionId, kind: "tui", peer: ui-<id>, address}
    ─────────────────────────────────────────────────► UiHost: one UiLeaf subtree
                                                        dial address ──────────► accept (once)
                                                        link ready
                                                        plugins.load("Tui", {…}) ─► Tui applies in this
                                                                                    process: raw mode,
                                                                                    hello / session.attach,
                                                                                    ui.attach {uiId, token}
    ◄ {uiId, generation}  (load settled)
user quits (/exit, Ctrl+D) in the TUI
  Leave → dispose the client tree (Tui restores the terminal; link closes)
                                                        link stopped → subtree disposed,
                                                        registration removed
  exit with the TUI's code; daemon keeps serving        later clients; stops on `daemon stop`
```

The normal `dimcode` process is itself the UI leaf: a rutis node in which
the daemon hosts the ordinary `Tui` plugin (§3.6). It keeps the terminal it
was started on, stays in the shell's foreground job, and returns the TUI's
exit code. `ui.open` is an internal daemon RPC used by this startup, not a
separate user command. A daemon without `ui.leaf` keeps the direct TUI.
Before a model is configured, initial setup uses that existing direct TUI (§3.6).

### 3.2 `/web` from the TUI: daemon-owned switch

```text
Tui                                   SessionHost                        daemon WebEntry
───                                   ───────────                        ───────────────
user types /web
Command::ToggleWeb  ────────────────►  web.toggle {port?}  ─────────────►  enabled = !enabled
                                         ◄ {enabled, open, url?, address?}
Update::Web {enabled, open, url}
  print URL / off notice in place     (TUI keeps running)
```

The TUI does not exit and does not become a holder. While the switch or a
foreground `dimcode web` holder keeps the entry open, the TUI reports the
URL; turning the switch off leaves it open if a holder remains. A foreground
`dimcode web` still calls `web.open` and holds the entry until it exits, and
`dimcode web --off` clears the switch without disconnecting that holder.
`/web` in a daemon-owned leaf therefore has the same meaning and never
requires a terminal hand-off.

### 3.3 One-shot actions and `daemon stop`

```text
dimcode exec / export / login / models / mcp …        dimcode daemon stop
──────────────────────────────────────────────        ───────────────────
root.plugin(DaemonLink)  → Connection                  DaemonLink{start: false}:
take the Client, call the action's RPCs on it                         no socket → "Daemon is not running." exit 0
print; root.dispose(); exit                              socket → hello; daemon.shutdown;
                                                         wait for EOF (≤ 30 s); exit 0
```

`exec` without a terminal, `--prompt`, `--no-tui` and piped stdin take this path; the `Tui` plugin is never loaded.

### 3.4 Daemon side, for reference

Unchanged. `serve` builds the daemon tree, loads the loader, reconciles the assembly layer (Rust process rows, including the managed provider plugin in an account build) within `startup_timeout`, then listens. A client that connects during assembly waits in `DaemonLink`'s poll; the socket exists only after assembly.

<a id="35-stop-one-background-command-or-member"></a>

### 3.5 Background command output and member interruption

`/tasks` (`/background`) lists the Session's running commands and members from
the existing event projection. The transcript remains on main; task identities
retain their originating branch, with independent event cursors per branch.
Enter opens a command's output detail; `x` stops that command through the existing
`session.exec.stop`. Both read and stop use its actual branch.

The [command-output contract](https://github.com/arcships/dim/blob/main/docs/design/background-task-output.md)
uses WorkspaceTools' optional C1 method `session.exec.output`. This UI consumes
that read when provided; a profile without it displays an unavailable state. It never opens a
daemon path or falls back to a tool call.

```text
+-- Background tasks -------------------------------------------+
| > cargo build                                  command         |
|   Check the docs                               agent           |
| Enter output / interrupt member   x stop command   Esc close   |
+---------------------------------------------------------------+

+-- cargo build ------------------------------------------------+
| Tail preview - last 2,000 lines / 50 KiB                        |
| Compiling dim-app-server                                      |
| ...                                                           |
| x stop command   Esc back                                     |
+---------------------------------------------------------------+
```

An open command detail reads immediately, then one second after each response.
Only one read is outstanding, including a read admitted before the detail closes.
Closing, switching away, or a terminal event stops refreshes; a late reply cannot
replace another detail. Reopening permits a new attempt after an error. Loading
says `Loading output...`; an empty active preview says `No output yet`.
`exec_task_unknown` retains the last preview and shows `Live output unavailable`,
without fabricating an end. A missing method says `Live output unavailable in this
profile`. Other errors retain the preview and show their reason. Only the real
`exec/ended` supplies terminal status and the original final result. The finished
task projection reaches the list and detail together, so reopening a completed
task uses its recorded result without starting another live read. The finished
detail stays until Esc; the running list removes the task as before. Output wraps
within the existing overlay, shows the tail by default, and supports
Up/Down, PageUp/PageDown and Home/End. Narrow layouts preserve the status and key
hints; the existing minimum-size screen still applies. Output, read errors and
the command-detail title use the existing terminal-text sanitizer before styling.

For a member, `collaboration/delivered` binds the request to its actual Session.
Enter interrupts that Session's main turn through `session.stop`. Starting rows
explain that no target is available yet; rejected deliveries remove their
placeholder. Only the real collaboration result removes a delivered row. A stop
reply does not close the Session, retract adopted input or guarantee cancellation
of a particular delegation; a later turn may race it. RPC errors use the existing
notice path. Member conversation/progress, model policy and stop-all remain #263;
recently ended task retention and Workflow presentation remain #264. The controls
use the existing localization/theme paths and add no server method, event, lock
or plugin.

### 3.6 UI registry and UI leaves

The daemon is the authority over which interfaces are alive. A terminal
interface is a **UI leaf**: the interactive `dimcode` process is a rutis
node linked to the daemon, in which the daemon hosts the ordinary `Tui`
plugin. This is rutis node interconnection (rutis
[remote plugins §5](https://github.com/arcships/rutis/blob/main/docs/design-remote-plugins-2026-10-03.en.md#5-full-node-interconnection),
[nodes](https://github.com/arcships/rutis/blob/main/docs/guide/nodes.en.md)):
the leaf opens `host` for its daemon, and the daemon loads `Tui` there as a
row it controls, unloads it, or replaces it with another leaf's. Lifecycle
and liveness are rutis facts (the link and the hosted load); there is no
UI-specific control protocol. Business traffic stays on protocol v3: the
hosted `Tui` is the same client as before. One executor, many viewers: a
Session runs in the daemon only; every interface, terminal or browser,
submits input and observes events; none executes. `UiHost` writes no
Timeline fact.

```text
daemon rutis root                                  dimcode (the leaf, shell foreground)
  UiHostPlugin (process Capability)                  client tree (rutis root)
    ├─ registry: uiId -> {sessionId, kind,             ├─ leaf transport: private socket,
    │    generation, connection, phase}                │    accepts one connection
    ├─ Transport#ui-leaf (dials leaf sockets)          └─ PeerPlugin: link to dim-daemon
    └─ UiLeaf (one subtree per leaf)                        features: host { "Tui" }
         ├─ PeerPlugin: link to ui-<id>                          └─ Tui (hosted row)
         │    features: none today                                    ├─ DaemonLink
         │    (later: import ui/*)                                    └─ render + session threads
         └─ TuiRow: await plugins offer, load "Tui" {config},               protocol v3 ──► daemon Rpc
                    unload in its effect
```

The daemon does not start the leaf process: the user's shell does, and the
process keeps the terminal it was started on. The daemon controls what runs
in it. This keeps the terminal with the process the shell's job control
knows (`/suspend`, Ctrl+Z and resume work as in a shell-started TUI), and
leaves the daemon nothing to do with terminals, processes or their exit
codes.

**Link.** Before `ui.open`, the leaf builds its node in its client tree:
a leaf transport bound to a private address, its identity `ui-<id>` (a
fresh id it chooses), and a `PeerPlugin` whose only feature is `host` with
`Tui` in its catalog. On Unix the address is a socket `ui-<8 hex>` in the
native home, beside `daemon.sock` and of the same length, so any home that
fits the daemon's socket path fits the leaf's; on Windows it is a loopback
TCP address that requires a one-time token as its first line. The leaf accepts exactly
one connection and then removes the socket. The token is read byte by byte, so the bytes after
its newline stay for the session. `ui.open {peer, address}` makes
the daemon mount a `UiLeaf`: a `PeerPlugin` that dials `address` with the
link expecting `peer` (the endpoint handshake checks it), without reconnect,
then `TuiRow`. Both links are the same rutis link; nothing is spawned.
Access to the address is the authorization, as for `daemon.sock`: the
directory and socket are private to the user, and `ui.*` is local-only.

**Hosted `Tui`.** `TuiRow` waits for the leaf's `plugins` family in
`Peer::offers()` before sending `plugins.load(key, "Tui", config)`: the
link can be ready before its host feature is active. This wait shares
`ui.open`'s existing startup deadline. It then
registers `plugins.unload(key)` as its effect; an unload after the link
ended counts as done (the hosted row went with the link). This is what the
loader's `peer:` row does; dim keeps its own small row because the loader's
is private and rows of the loader are persistent configuration, while a UI
leaf exists per `ui.open`. The config is only what the daemon decides:

| Field | Meaning |
| --- | --- |
| `uiId` | The registration this leaf is (its node id) |
| `token` | One-time attach token for `ui.attach`; the daemon keeps only its SHA-256 digest |
| `sessionId` | The Session to show |

Everything else (terminal mode flags, workspace, product home, model
configuration, endpoint) is the leaf process's own invocation, as for a
shell-started TUI. The hosted `Tui` connects with its own `DaemonLink`
(`start: false`) and runs as in §1. Its `apply` returns once the render and
session threads run; the load settles then.

**Leaving.** When the user quits, the leaf disposes its client tree: the
`Tui` effect restores the terminal and the link closes. The daemon sees the
link stop, disposes the `UiLeaf` and removes the registration; the process
exits with the TUI's code. When the daemon closes the leaf instead
(`ui.close`, replacement, daemon shutdown), it sends the advisory
`ui.shutdown` notice, then disposing `UiLeaf` unloads `Tui` (terminal
restored by the same effect) and closes the link; the leaf process exits
when its link stops. A forced daemon exit closes the link without the
unload; the leaf still disposes its tree when the link stops. No process
outlives anything: the leaf process is the user's, and it ends with its
link.

| Method (local connections, protocol v3) | Meaning |
| --- | --- |
| `ui.open {sessionId, kind, peer, address, replace?, expectedGeneration?}` → `{uiId, generation}` | Links to the leaf node at `address` and hosts `Tui` there. The caller must be attached to the Session. `kind` is `tui`; `peer` is the leaf's node id (`ui-` and lowercase letters, digits and `-`; a registered id is `invalid_params`) and becomes `uiId`. Returns once the hosted `Tui` load settled; a link or load failure is `ui_leaf_failed` and leaves no registration. `replace` names an interface of the same Session to close after the new leaf is loaded; `expectedGeneration` (only with `replace`) is the generation the caller observed. The old registration is claimed before the link, so a second replacement of the same generation fails `ui_generation`; a failed start releases the claim and leaves the old interface running. |
| `ui.attach {sessionId, kind, uiId?, token?}` → `{uiId, generation}` | Registers the calling connection as one interface of `sessionId` (`session_not_attached` otherwise). A leaf's `Tui` passes its `uiId` and token (a wrong token is `ui_token`; another kind than the leaf's is `invalid_params`); an ordinary interface passes neither and gets a connection registration. A leaf whose TUI switches Session (`/new`, resume) calls it again and the registration follows. Not exclusive: a Session has as many viewers as connections attach. |
| `ui.status {}` → `{instances: [{uiId, sessionId, kind, sinceMs, generation, state}], webConnections}` | A read for management clients: every registered interface, and the count of browser connections, which are not registered. Nobody waits on it. |
| `ui.close {uiId, reason?}` → `{}` | Removes the registration: the advisory notice on its connection, and for a leaf the `UiLeaf` disposal (unload, then link close). |
| `ui.shutdown {uiId, reason?}` → `{}` | The advisory notification only. A leaf whose connection is between a detach and its reattach has none to address (`ui_detached`). |
| `ui.replace {uiId, sessionId, kind, generation?}` → `{uiId, generation}` | For connection registrations: the caller registers anew and the old `uiId` is closed afterwards, under the same claim as `ui.open {replace}`. |

A connection registration is `online`. A leaf's state comes from facts that
already exist:

| State | Fact |
| --- | --- |
| `starting` | `UiLeaf` is linking or loading, or `Tui` is loaded and its connection has not attached yet |
| `online` | `Tui` loaded and its connection attached through `ui.attach` |
| `replacing` | online, and it is the new side of a replacement not finished yet |

A leaf whose link stops is removed; there is no `offline` leaf to keep. A
detach while the leaf runs (`/new`, resume) does not change its state. A
failed start is not a state: `ui.open` fails and nothing is registered.
There is no heartbeat: liveness is the link's. A TUI that hangs while its
process lives stays `online`; `ui.close` still unloads it within a bounded
wait and closes the link.

**Ownership.** The link is the owner: a leaf lives as long as its link.
The connection that called `ui.open` has no further role. Daemon shutdown
disposes `UiHostPlugin`, which waits for every `UiLeaf` to be disposed.

**Generations** are monotonic per daemon. `ui.open` returns one,
`ui.status` lists them, and replacements check the one the caller observed.
A replacement is not atomic across processes: the new leaf stands when the
old close fails, and no rollback is claimed.

`/web` toggles the daemon-owned web switch, so the TUI stays in the
terminal. `/suspend` and "Restart now" work as in a shell-started TUI: the
leaf is the shell's process.

`hello` advertises `ui.*` to local connections only, with the internal
`ui.leaf` feature; a client of an older daemon keeps the direct-TUI path.
Remote devices are not leaves (`DeviceLease`/RemoteGateway); browsers are
counted viewers.

**Initial setup before a Session.** When an untargeted product startup has no
usable model, `session.create` returns `provider_failed` before creating a Session.
There is no Session for a leaf to attach to. This entry uses the existing direct
TUI for its lifetime, keeping `/connect` and `/login` available. A Session later
created by that TUI uses the existing connection registration; the next normal
startup uses a leaf. Explicit model targets, `--model-config`, resume and other
error codes retain their errors. No fake model, implicit model substitution, new
protocol or background setup process is introduced.

```text
session.create -- usable model --> ui.open --> hosted Tui
               ` no model -----> direct Tui --> /connect or /login
```

#### Later: UI events toward the agent (not implemented)

The leaf link is where interface changes will reach the daemon. With rutis
0.16 events are one flat bus per root and node links carry them by name,
gated by `export` on the emitting side and `import` on the listening side.
The contract reserved here:

- Names under `ui/` (`ui/<what>`), emitted on the leaf's bus by the hosted
  `Tui`; the leaf's link exports `ui/*` and `UiLeaf`'s link imports `ui/*`.
  Turning this on changes only the two `Features`; the link keeps its session.
- Payloads carry `uiId` and `sessionId` and are serde values (0.13+ events
  require it).
- A daemon capability that wants them into a Session turns them into
  adopted inputs (rule 10). No `ui/*` event enters model context by itself,
  and none writes the Timeline directly.

The import is wired as its own change, not part of the
[0.16 upgrade](../rutis-0.16-upgrade.md).

## 4. Timing and conflicts

| Case | Decision |
| --- | --- |
| Two clients start at once with no daemon | Both spawn; the second daemon fails to bind and exits; both clients connect to the survivor within the poll window. Unchanged behaviour, now inside `DaemonLink::apply`. |
| Daemon still assembling when the client polls | The socket does not exist yet; the client keeps polling until the deadline. The daemon's `startup_timeout` is independent of the client's 10 s; a slow managed-plugin start surfaces as "App Server startup timed out; see daemon.log". |
| Connection drops while the TUI runs | **Not a provider replacement.** The TUI keeps its draft and reports the failure. There is no automatic reconnect in the session thread. |
| `apply` of `Tui` fails after raw mode was entered | The terminal effect was registered before raw mode, so rutis runs it on the failed load; the error is printed on a restored terminal. |
| Ctrl+C in the TUI | A key event in raw mode, handled by the render thread (cancel the turn or quit), as today. A SIGTERM or SIGHUP reaches the leaf process like any shell-started TUI; its terminal guard restores, and the link closes with the process. |
| Daemon exits while a client holds `Client` | A direct TUI shows the failure and keeps running until the user quits. A leaf's link stops with the daemon, so the leaf disposes its tree (terminal restored) and exits. No automatic daemon restart from a running client. |
| `daemon stop` while another client is attached | The daemon shuts down after settlement (`daemon.shutdown.completed`); disposing `UiHostPlugin` unloads each hosted `Tui` (terminal restored) before closing its link, and each leaf process then exits. |
| `ui.open` start fails | Nothing is registered; a `replace` leaves the old one running and is never reported as an atomic rollback. The leaf process falls back to an error on a restored terminal. |
| `ui.close` of a hung leaf | The unload does not settle; the link is closed after a bounded wait and the registration is gone. The leaf's own task sees the link stop and exits the process even when its render thread hangs; only a process that is stopped (SIGSTOP) stays until it continues, and the daemon never signals a process it did not start. The Session is untouched. |
| User quits a leaf | The leaf disposes its tree and exits with the TUI's code; the link stops and the registration is removed. |
| Leaf process killed | Its link stops; the daemon disposes the `UiLeaf` and removes the registration. |
| Daemon shutdown with leaves | Graceful: disposing `UiHostPlugin` waits for every `UiLeaf` disposal (unload, then link close). Forced: the links close without the unload; each leaf disposes its tree when its link stops. |
| Replacing the executable while a daemon runs | Unchanged: the server link is remade on the next start; `daemon stop` with the matching CLI first ([release packaging](../../release/README.md#upgrade-compatibility)). |

### Connection loss and terminal content

A failed Session worker retains the transcript, current detail and draft, but accepts no more
keyboard submissions or Session controls. Escape closes an open detail; the draft remains editable. The interface stops its running indicator, reports a disconnected
state and prints `dimcode --resume <session-id>` (or `dimcode` before a Session exists).
Ctrl+C, Ctrl+D or `/exit` leaves. Remote work is not reported completed or cancelled;
a shell command's outcome remains unknown. There is no automatic reconnect.

```text
+-- Disconnected · draft kept ---------------------------------+
| Exit and run: dimcode --resume <session-id>                  |
| > retained draft                                             |
+--------------------------------------------------------------+
```

Only one interactive `!` / `!!` command runs at once. A second command is refused with a
notice and retains its draft; the first run keeps its progress, result and stop identity.
Tool and shell text loses terminal escape/control sequences before layout; only the
renderer adds terminal styling. The write path keeps that promise for every row, model
text included: of the sequences a row carries, only SGR and an OSC 8 link to an `http`,
`https` or `mailto` target reach the terminal. Clicked links use only those schemes.

## 5. Context mapping

None. No client plugin writes to any Timeline or produces model context; user input reaches the model only through `session.submit`, as today.

## 6. Interfaces

| Interface | Form |
| --- | --- |
| `Connection` service (client tree) | Rust: `Arc<Connection>` keyed by `TypeKey::of`, provided by `DaemonLink`: a take-once slot holding the protocol v3 `Client` (`rpc`, `typed`, notifications) after `hello`. `Client` is driven through `&mut self` by one owner, so the service hands it over (`take`) rather than sharing it: the session thread of `Tui`, the holding task of `WebHold`, or the one-shot action. `take` yields `None` once taken, or when `start: false` found no daemon (`daemon stop`). The language-neutral contract is the wire protocol itself ([protocol.rs](../../../crates/dim-app-server/src/protocol.rs)); no other language runs in the client tree, so no named binding is defined ([D5](../../adr/002-plugin-scope.md#d5-contracts-are-language-neutral) as in AcpGateway). |
| `Tui` result | `Leave::Exit(i32)` or `Leave::Restart` delivered to the composition root through a one-shot channel the plugin is constructed with. |
| `WebHold` input | `port: Option<u16>`, `open_browser: bool`; its result (`Ok(0)` on a signal, the error on the daemon's end) goes to the composition root through a one-shot channel, like `Tui`'s. It holds the entry open; `/web` never loads it. |
| Daemon RPC used | Existing `hello`, `session.create/attach/submit/history`, `approval.respond`, `web.open`/`web.toggle`/`web.disable`/`web.status`, `daemon.shutdown` and the existing command/member stop paths; optional `session.exec.output` reads the command's bounded capture; `ui.open`/`ui.close`/`ui.status`/`ui.replace` for management, `ui.attach` after each `session.attach` when offered, and the `ui.shutdown` notification (§3.6). |
| Hosted `Tui` (leaf) | rutis host catalog entry `Tui` in the leaf's client tree; its config is the table in §3.6, validated by the entry's schema. Rust-only: no other language hosts it. |

## 7. Distribution

Unchanged artifacts; the plugins are compiled into the client and ship with it, which is rutis's own model for application-hosted plugins ("plugins are compiled into the application and released with it").

| Artifact | Content | Change |
| --- | --- | --- |
| Six native archives (`dimcode-<platform>`) | One `dimcode` executable: CLI with the client tree (which is also the UI leaf node), App Server as the `app-server` subcommand; the Web workspace is the separate `dimcode-web` artifact ([web-workspace §2.4](../web-workspace.md#24-assets)) | `dim-cli` gains `rutis-bridge` for the leaf node and keeps its existing `rutis` dependency. No loader row and no Node runtime are added |
| Account build | `dimcode` built with the Dim account's client IDs: the same CLI and daemon with the Dim account plugin installed through `cli::run_with` | None. Neither tree loads Node |
| Seven npm tarballs (`dimcode` wrapper + six platform packages) | Wrapper picks the platform package and runs the executable | None |
| `release-manifest.json`, `SHA256SUMS` | Provenance | None |
| Development: `./dev` | Build the executable, stage the local plugin archive, start the daemon through the CLI | None; the CLI still spawns the daemon the same way |

No loader row is added, so there is nothing to enable, disable or persist ([D4](../../adr/002-plugin-scope.md#d4-structural-configuration-and-business-configuration-are-separate)). Normal `dimcode` startup uses the daemon's internal `ui.open` management RPC; there is no `dimcode ui open` command. Whether the TUI or ACP front end runs is decided by the subcommand, as today. A client loader with `Builtins` rows (so that front ends could be toggled by configuration) is possible later without changing this document's plugins; it is out of scope.

## 8. Security

The client tree runs trusted in-process code only; a plugin boundary is not a sandbox ([rule 11](../../../AGENTS.md)). `DaemonLink` keeps every existing check: private native home (0700, owner), private log file, `O_NOFOLLOW`, socket permission race, server link identity. A leaf's address is private to the user: a socket in the native home (0700, owned by the user, as for `daemon.sock`), accepted once and removed (on Windows a loopback address that requires a one-time token first). The daemon dials only addresses a local `ui.open` caller names, and the link's endpoint handshake checks the leaf id. The attach token for `ui.attach` travels only in the hosted `Tui`'s load config over that link; the daemon stores its SHA-256 digest, and nothing UI-specific is in any environment. The leaf link exposes no Session or management method and is not the business channel. Tokens are never logged or written to the Timeline. The sign-in link printed by `WebHold` or reported by the TUI is shown only when no browser took it, as today. Nothing here changes approval, permission or the product-home boundary.

## 9. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (Capability, `Deps`, `apply`, effects) | `UiHostPlugin` is a process `Capability` providing `UiHost` and the transport that dials leaf addresses. Each leaf is one `UiLeaf` plugin whose children are the `PeerPlugin` link and `TuiRow`; `TuiRow` injects the link's `Peer` and registers the unload as its effect, so disposing `UiLeaf` releases everything in reverse order. In the leaf, the client tree mounts the leaf transport and `PeerPlugin`; the hosted `Tui` registers its terminal restore before raw mode. |
| §4 rule 2 (critical effects through the Timeline) | Not applicable: no client plugin produces or consumes Timeline events; input goes through `session.submit`. Future `ui/*` events enter a Session only as adopted inputs (§3.6). |
| §4 rule 3 (handlers register tasks and return) | `Tui::apply` starts its threads and returns; the hosted load settles then. No handler waits on the business connection. |
| §4 rule 4 (state ownership, Timeline writes) | The registry is `UiHost`'s; a leaf's state is derived from the link, the load and the attachment, not reported. Client state is view state owned by the TUI. |
| §4 rule 5 (crate restrictions, `xtask check`) | `dim-cli` adds `rutis-bridge` for the leaf node; run `cargo run -p xtask -- check` and `graph`. The App Server graph records `UiHost`. |
| §4 rule 6 (tests follow requirements) | The fake-daemon CLI flow tests keep the direct-TUI fallback for older daemons; `ui_leaf.rs` covers normal `dimcode` startup as a leaf, `/new` keeping one registration, `/web` without leaving, exit code, replacement generations, a killed leaf leaving the registry, `ui.close` and daemon shutdown restoring the terminal and ending the process. |
| §4 rule 7 (reuse mechanisms) | rutis node interconnection (`PeerPlugin`, `host`, `plugins.load/unload`) instead of a UI control protocol; `Tui`, `DaemonLink`, `SessionHost` unchanged in substance. |
| §4 rule 8 (settle before removing services) | `UiLeaf` unloads the hosted `Tui` (terminal restored) before it closes the link. |
| §4 rule 9 (publish after commit) | Not applicable: no Timeline writes. |
| §1 (Capability and App Server boundary) | The leaf is the CLI process and its rutis root; it touches App Server business state only over protocol v3. Transport, Connection and Rpc gain only the `ui.*` built-ins and one notification; no Timeline fact. |
| ADR 002 D1 (scope follows the owner) | `UiHostPlugin` and every `UiLeaf` live in the daemon's process scope; the hosted `Tui` lives in the leaf, the process the shell started on that terminal. |
| D2 (no wiring modules) | No adapter plugin: `TuiRow` is the row that loads the hosted plugin, as the loader's `peer:` row is. One-shot actions stay functions. |
| D3 (dependencies point to the core) | The leaf depends on the daemon's public protocol and on the rutis host contract; no daemon plugin knows `Tui`'s internals, only its catalog name and config. |
| D4 (structural vs business configuration) | No loader rows; a leaf exists per `ui.open`. Terminal mode, workspace and input history remain the leaf invocation's own. |
| D5 (language-neutral contracts) | The business contract is the wire protocol; the hosted `Tui` config is a JSON schema; `ui/*` events will be serde values. |

## 10. Requirements raised to mechanism owners

None. Two rutis limits are accepted rather than raised:

- The loader's `peer:` row is private, so dim keeps its own `TuiRow` (load on
  apply, unload in the effect, an unload after the link ended counts as done).
- rutis's local transport does not listen, so the leaf side keeps a small
  transport that accepts one connection on its private address and the
  daemon side one that dials it; both produce ordinary rutis channels.

## Appendix A. Implementation notes

- `run_cli` keeps the `tokio` runtime it builds; the rutis root lives on it. The TUI's blocking loop moves from the main thread to the `dim-tui` thread spawned in `Tui::apply` (as `rutis-agent`'s `TuiPlugin` does); the main thread blocks on the root until `Leave` resolves.
- `SessionHost` becomes the session thread inside `Tui`; `SessionPort` stays the interface between `ui` and the session thread, unchanged.
- `DaemonLink` takes `Options` by value and a `start: bool` (false only for `daemon stop`).
- Terminal restoration is registered as the first effect, before entering raw mode, so a failed load still restores the terminal.
- The new module sits at the top of `dim-cli`, beside `lib.rs`: it uses `client`, `session_host` and `ui::tui`, which the `xtask` layer rules allow (`session_host` must not use `ui`, and `ui` must not use the line client; neither is touched).
- Actual change: one module `crates/dim-cli/src/plugins.rs` (`Connection`, `DaemonLink`, `Tui`, and the `WebHold` used by `dimcode web`), `run_cli` rewritten to build and dispose the root, `SessionHost::start` taking the connection instead of connecting, and `dim-cli/Cargo.toml` gaining `rutis.workspace = true`. `/web` is a `web.toggle` call in `SessionHost` and no longer unloads `Tui`. Under `ui/` only the restore surface and the Web report are touched; no loader row or daemon plugin is added.
