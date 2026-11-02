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
  │     │                              │    │ v3  │   ├─ session-node, py (sessions)     │
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
                                                    (process rows, session-node and py →
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
  LeafNode (same process): private socket, LocalId ui-<id>, accepting
  ui.open {sessionId, kind: "tui", peer: ui-<id>, address}
    ─────────────────────────────────────────────────► UiHost: one UiLeaf
                                                        dial address ──────────► accept (once)
                                                        handshake ◄────────────► handshake
                                                        (far id ui-<id>)         (far id dim-daemon)
                                                        session mounted          session mounted
    ◄ {uiId, generation, token}
  Tui (this process's own): raw mode,
    hello / session.attach, ui.attach {uiId, token} ──► online
user quits (/exit, Ctrl+D) in the TUI
  Leave → dispose the client tree (Tui restores the terminal; session closes)
                                                        session ended → UiLeaf disposed,
                                                        registration removed
  exit with the TUI's code; daemon keeps serving        later clients; stops on `daemon stop`
```

The normal `dimcode` process is itself the UI leaf: a rutis kernel in
session with the daemon, which runs the ordinary `Tui` plugin (§3.6). It
keeps the terminal it was started on, stays in the shell's foreground job,
and returns the TUI's exit code. `ui.open` is an internal daemon RPC used by
this startup, not a separate user command. A daemon without `ui.leaf.2`
keeps the direct TUI. Before a model is configured, initial setup uses that
existing direct TUI (§3.6).

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
the existing event projection. The list is the Session's, whichever line's
conversation is shown; task identities retain their originating branch, with
independent event cursors per branch.
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

The daemon is the authority over which interfaces are alive, and can close
or replace them. A terminal interface is a **UI leaf**: the interactive
`dimcode` process is a rutis kernel in a rutis session with the daemon
([rutis 0.19 upgrade M8](../rutis-0.19-upgrade.md#m8-the-ui-leaf-is-a-plugin-whose-life-is-a-session)),
and runs its own ordinary `Tui` plugin. The daemon's `UiLeaf` is a plugin
whose life is that session. Lifecycle and liveness are rutis facts (the
session and its end); there is no UI-specific control protocol. Business
traffic stays on protocol v3: the leaf's `Tui` is the same client as
before. One executor, many viewers: a Session runs in the daemon only;
every interface, terminal or browser, submits input and observes events;
none executes. `UiHost` writes no Timeline fact.

```text
daemon root (LocalId dim-daemon)                 dimcode leaf root
  UiHostPlugin (process Capability)                LeafNode
    ├─ registry: uiId -> {sessionId, kind,           ├─ LocalId ui-<id>
    │    generation, connection, state}              ├─ accept once, pipe::framed
    └─ UiLeaf (ctx.plugin, one per ui.open)          │   handshake: far id must be dim-daemon
         ├─ ctx isolated for catalog_key()           └─ session ── its end disposes the client tree
         ├─ dial address ──── one channel ─────────►
         ├─ handshake: far id must be ui-<id>
         └─ session ◄═════════ rutis session ═══════► (the leaf's session)
                                                   Tui (mounted by the leaf, config from ui.open's reply)
                                                     └─ DaemonLink ── protocol v3 ──► Rpc (ui.attach)
```

The daemon does not start the leaf process: the user's shell does, and the
process keeps the terminal it was started on. This leaves the daemon nothing
to do with terminals, processes or their exit codes.

**Session.** Before `ui.open`, the leaf binds a private address and mounts
`LeafNode` in its client tree, accepting there. `LeafNode` provides the
process's `LocalId` `ui-<id>` (a fresh id it chooses) and offers no plugin
catalog, so the daemon can run nothing on the leaf. On Unix the address is
a socket `ui-<8 hex>` in the native home, beside `daemon.sock` and of the
same length, so any home that fits the daemon's socket path fits the
leaf's; on Windows it is a loopback TCP address that requires a one-time
token as its first line, before the handshake. The leaf accepts exactly one
connection and then removes the socket. The token is read byte by byte, so
the bytes after its newline stay for the session. `ui.open {peer, address}`
makes the daemon mount a `UiLeaf`, which dials only that address, greets
from a context where the daemon's session catalog (`catalog_key()`) is
isolated, so the leaf can run nothing on the daemon, and drops a far end
whose id is not `peer`. The leaf drops a far end whose id is not
`dim-daemon`. Ids in the handshake are self-declared: access to the address
is the authorization, as for `daemon.sock` (the directory and socket are
private to the user, and on loopback TCP the token authenticates the leaf),
and `ui.*` is local-only. Both sides then mount the session; nothing is
spawned. Because `LeafNode` accepts before `ui.open` is called, the
handshake completes while `ui.open` is in flight.

**The leaf's `Tui`.** Once `ui.open` replies, the leaf mounts its own `Tui`.
It takes from the reply only what the daemon decides:

| Field | Meaning |
| --- | --- |
| `uiId` | The registration this leaf is (its id) |
| `token` | One-time attach token for `ui.attach`; the daemon keeps only its SHA-256 digest |

and `sessionId` from its own `ui.open`. Everything else (terminal mode
flags, workspace, product home, model configuration, endpoint) is the leaf
process's own invocation, as for a shell-started TUI. The `Tui` connects
with its own `DaemonLink` (`start: false`) and runs as in §1. A `Tui` that
fails to load or ends with an error disposes the client tree as below: the
leaf exits nonzero, its session ends, and the daemon removes the
registration.

**Leaving.** The session is the leaf's lifetime. Mounting the session gives
its fiber on each side; its end (Disposed or Failed) is the fact, and no
poll or timer observes it. When the user quits, the leaf disposes its
client tree: the `Tui` effect restores the terminal and the session closes;
on the daemon the session's end disposes `UiLeaf` and `UiHost` removes the
registration. The process exits with the TUI's code. When the daemon closes
the leaf instead (`ui.close`, replacement, daemon shutdown), it sends the
advisory `ui.shutdown` notice, then disposes `UiLeaf`, which closes the
session; the leaf's session end disposes its tree (terminal restored by the
same effect) and the process exits. A forced daemon exit ends the session
as well. No process outlives anything: the leaf process is the user's, and
it ends with its session.

| Method (local connections, protocol v3) | Meaning |
| --- | --- |
| `ui.open {sessionId, kind, peer, address, replace?, expectedGeneration?}` → `{uiId, generation, token}` | Opens a session with the leaf at `address`. The caller must be attached to the Session. `kind` is `tui`; `peer` is the leaf's id (`ui-` and lowercase letters, digits and `-`; a registered id is `invalid_params`) and becomes `uiId`. Returns once the session is mounted, with the one-time `token` the leaf's `Tui` attaches with; it fails only on the link or the handshake, as `ui_leaf_failed`, and leaves no registration. A leaf that does not attach within the startup bound (`START`, 20 s, counted from the reply) is closed as for `ui.close`. `replace` names an interface of the same Session to close once the new leaf attaches; `expectedGeneration` (only with `replace`) is the generation the caller observed. The old registration is claimed before the dial, so a second replacement of the same generation fails `ui_generation`; a failed start, or a new leaf that ends or reaches the bound before it attaches, releases the claim and leaves the old interface running. |
| `ui.attach {sessionId, kind, uiId?, token?}` → `{uiId, generation}` | Registers the calling connection as one interface of `sessionId` (`session_not_attached` otherwise). A leaf's `Tui` passes its `uiId` and token (a wrong token is `ui_token`; another kind than the leaf's is `invalid_params`); an ordinary interface passes neither and gets a connection registration. A leaf whose TUI switches Session (`/new`, resume) calls it again and the registration follows. Not exclusive: a Session has as many viewers as connections attach. |
| `ui.status {}` → `{instances: [{uiId, sessionId, kind, sinceMs, generation, state}], webConnections}` | A read for management clients: every registered interface, and the count of browser connections, which are not registered. Nobody waits on it. |
| `ui.close {uiId, reason?}` → `{}` | Removes the registration: the advisory notice on its connection, and for a leaf the `UiLeaf` disposal, which closes its session. |
| `ui.shutdown {uiId, reason?}` → `{}` | The advisory notification only. A leaf whose connection is between a detach and its reattach has none to address (`ui_detached`). |
| `ui.replace {uiId, sessionId, kind, generation?}` → `{uiId, generation}` | For connection registrations: the caller registers anew and the old `uiId` is closed afterwards, under the same claim as `ui.open {replace}`. |

A connection registration is `online`. A leaf's state comes from facts that
already exist:

| State | Fact |
| --- | --- |
| `starting` | From `ui.open` until the leaf's `Tui` calls `ui.attach {uiId, token}` |
| `online` | Its `Tui` attached through `ui.attach` |
| `replacing` | online, and it is the new side of a replacement still closing the old interface |

A leaf whose session ends is removed; there is no `offline` leaf to keep. A
detach while the leaf runs (`/new`, resume) does not change its state. A
failed start is not a state: `ui.open` fails and nothing is registered. No
registration stays without an interface: a leaf that does not attach
within the bound is closed. There is no heartbeat: liveness is the
session's. A TUI that hangs while its process lives stays `online`;
`ui.close` still closes its session.

**Ownership.** The session is the owner: a leaf lives as long as its
session. The connection that called `ui.open` has no further role. Daemon
shutdown disposes `UiHostPlugin`, which waits for every `UiLeaf` to be
disposed.

**Generations** are monotonic per daemon. `ui.open` returns one,
`ui.status` lists them, and replacements check the one the caller observed.
A replacement closes the old interface when the new leaf attaches, that is,
once the new one is `online`. It is not atomic across processes: the new
leaf stands when the old close fails, and no rollback is claimed.

`/web` toggles the daemon-owned web switch, so the TUI stays in the
terminal. "Restart now" works as in a shell-started TUI: the
leaf is the shell's process.

`hello` advertises `ui.*` to local connections only, with the internal
`ui.leaf.2` feature (the session protocol and `ui.open`'s reply changed
from `ui.leaf`). A client that does not find it keeps the direct-TUI path:
an old CLI with a new daemon, and a new CLI with an old daemon.
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
session.create -- usable model --> ui.open --> the leaf's own Tui
               ` no model -----> direct Tui --> /connect or /login
```

#### Later: UI events toward the agent (not implemented)

The leaf's session is where interface changes will reach the daemon. Events
are one bus across a rutis session, and listener names are announced in
both directions; there is no `export`/`import` gate and no prefix
subscription. `ui/*` events a leaf emits would therefore reach every daemon
listener of their names. The contract reserved here is unchanged:

- Names under `ui/` (`ui/<what>`), emitted on the leaf's bus by its `Tui`.
- Payloads carry `uiId` and `sessionId` and are serde values.
- A daemon capability that wants them into a Session turns them into
  adopted inputs (rule 10). No `ui/*` event enters model context by itself,
  and none writes the Timeline directly.

## 4. Timing and conflicts

| Case | Decision |
| --- | --- |
| Two clients start at once with no daemon | Both spawn; the second daemon fails to bind and exits; both clients connect to the survivor within the poll window. Unchanged behaviour, now inside `DaemonLink::apply`. |
| Daemon still assembling when the client polls | The socket does not exist yet; the client keeps polling until the deadline. The daemon's `startup_timeout` is independent of the client's 10 s; a slow managed-plugin start surfaces as "App Server startup timed out; see daemon.log". |
| Connection drops while the TUI runs | **Not a provider replacement.** The TUI keeps its draft and reports the failure. There is no automatic reconnect in the session thread. |
| `apply` of `Tui` fails after raw mode was entered | The terminal effect was registered before raw mode, so rutis runs it on the failed load; the error is printed on a restored terminal. |
| Ctrl+C in the TUI | A key event in raw mode, handled by the render thread (cancel the turn or quit), as today. A SIGTERM or SIGHUP reaches the leaf process like any shell-started TUI; its terminal guard restores, and the session ends with the process. |
| Daemon exits while a client holds `Client` | A direct TUI shows the failure and keeps running until the user quits. A leaf's session ends with the daemon, so the leaf disposes its tree (terminal restored) and exits. No automatic daemon restart from a running client. |
| `daemon stop` while another client is attached | The daemon shuts down after settlement (`daemon.shutdown.completed`); disposing `UiHostPlugin` closes each leaf's session, and each leaf disposes its tree (terminal restored) and exits. |
| `ui.open` start fails | Nothing is registered; a `replace` leaves the old one running and is never reported as an atomic rollback. The leaf process falls back to an error on a restored terminal. |
| `ui.close` of a hung leaf | The daemon closes the session and the registration is gone. The leaf's session ends, and the process exits even when its render thread hangs (the terminal restore waits a bounded time); only a process that is stopped (SIGSTOP) stays until it continues, and the daemon never signals a process it did not start. The Session is untouched. |
| User quits a leaf | The leaf disposes its tree and exits with the TUI's code; the session ends and the registration is removed. |
| Leaf process killed | Its session ends; the daemon disposes the `UiLeaf` and removes the registration. |
| Leaf `Tui` never attaches | Closed after `START` (20 s from `ui.open`'s reply), as for `ui.close`; a replacement it was making leaves the old interface running. |
| Daemon shutdown with leaves | Graceful: disposing `UiHostPlugin` waits for every `UiLeaf` disposal, each closing its session. Forced: the sessions end with the process; each leaf disposes its tree when its session ends. |
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
| Daemon RPC used | Existing `hello`, `session.create/attach/submit/history`, `session.branch_end`, `approval.respond`, `web.open`/`web.toggle`/`web.disable`/`web.status`, `daemon.shutdown` and the existing command/member stop paths; optional `session.exec.output` reads the command's bounded capture; `ui.open`/`ui.close`/`ui.status`/`ui.replace` for management, `ui.attach` after each `session.attach` when offered, and the `ui.shutdown` notification (§3.6). |
| The leaf's `Tui` | Mounted by the leaf itself once `ui.open` replies, with `uiId` and `token` from the reply (§3.6). The leaf offers no plugin catalog. |

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

The client tree runs trusted in-process code only; a plugin boundary is not a sandbox ([rule 11](../../../AGENTS.md)). `DaemonLink` keeps every existing check: private native home (0700, owner), private log file, `O_NOFOLLOW`, socket permission race, server link identity. A leaf's address is private to the user: a socket in the native home (0700, owned by the user, as for `daemon.sock`), accepted once and removed (on Windows a loopback address that requires a one-time token first). The daemon dials only addresses a local `ui.open` caller names; each side drops a far end whose handshake id is not the one it expects, and on loopback TCP the one-time token, sent before the handshake, is what authenticates the leaf. The daemon greets from a context where its session catalog is isolated, so the leaf can run no daemon plugin, and the leaf offers no catalog. The attach token for `ui.attach` travels only in `ui.open`'s reply, on the local connection; the daemon stores its SHA-256 digest, and nothing UI-specific is in any environment. Tokens are never logged or written to the Timeline.

**What the leaf's session carries.** A rutis session exports every process-group name that crosses languages to its far end, and announces listener names in both directions ([rutis 0.19 upgrade M8](../rutis-0.19-upgrade.md#m8-the-ui-leaf-is-a-plugin-whose-life-is-a-session)). The [generated catalog](../../architecture/rust-agent/generated/catalog.md) lists those names, and the installed foreign process rows add the names they provide. Today they are `auxiliaryModel`, `managementMethods`, `partRegistry`, `pluginFiles`, `sessionMetadata`, `sessionPermissions`, `settings` and `workflowFiles`; `sessions`, whose methods `launch`, `input`, `lookup`, `history`, `withdraw`, `stop`, `idle` and `delete` act on Sessions; and the default plugins' `scheduleJobs` and `workflowRuns`. Code in the leaf could therefore act on any Session, register management methods, and register Session parts whose copies run in the leaf; it could emit any event name a daemon listener has, and that listener would receive it. The leaf's code does none of this: its `Tui` uses protocol v3 only and emits no event. This is accepted because the leaf is reached only through a private address that a local `ui.open` caller names, and that caller already holds a local protocol v3 connection on `daemon.sock` reaching the same Sessions and management methods; the trust boundary stays the local user ([ADR 003](../../adr/003-approval-scope.md), [rule 11](../../../AGENTS.md)). These services are not isolated from the leaf: they are Dim's shared cross-language services, which the repository's own TypeScript plugins inject, and a session exports to every far end alike, the Node runtime's included.

Business traffic stays on protocol v3; the leaf's session is not the business channel. The sign-in link printed by `WebHold` or reported by the TUI is shown only when no browser took it, as today. Nothing here changes approval, permission or the product-home boundary.

## 9. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (Capability, `Deps`, `apply`, effects) | `UiHostPlugin` is a process `Capability` providing `UiHost`. Each leaf is one `UiLeaf` plugin whose child is the session, so disposing `UiLeaf` closes the session and its channel. In the leaf, the client tree mounts `LeafNode`, whose child is the session, and the leaf's `Tui`, which registers its terminal restore before raw mode. |
| §4 rule 2 (critical effects through the Timeline) | Not applicable: no client plugin produces or consumes Timeline events; input goes through `session.submit`. Future `ui/*` events enter a Session only as adopted inputs (§3.6). |
| §4 rule 3 (handlers register tasks and return) | `Tui::apply` starts its threads and returns. `LeafNode` accepts in a task of its own, so it is mounted and accepting before `ui.open`. No handler waits on the business connection. |
| §4 rule 4 (state ownership, Timeline writes) | The registry is `UiHost`'s; a leaf's state is derived from its session and its attachment, not reported. Client state is view state owned by the TUI. |
| §4 rule 5 (crate restrictions, `xtask check`) | `dim-cli` adds `rutis-bridge` for the leaf node; run `cargo run -p xtask -- check` and `graph`. The App Server graph records `UiHost`. |
| §4 rule 6 (tests follow requirements) | The fake-daemon CLI flow tests keep the direct-TUI fallback for older daemons; `ui_leaf.rs` covers normal `dimcode` startup as a leaf, `/new` keeping one registration, `/web` without leaving, exit code, replacement generations, a killed leaf leaving the registry, `ui.close` and daemon shutdown restoring the terminal and ending the process. |
| §4 rule 7 (reuse mechanisms) | A rutis session (`Session::handshake`, the mounted `Greeted`, its fiber's end) instead of a UI control protocol; `Tui`, `DaemonLink`, `SessionHost` unchanged in substance. |
| §4 rule 8 (settle before removing services) | The leaf disposes its `Tui` (terminal restored) before its session closes; the daemon closing a leaf closes the session, whose end disposes the leaf's tree in the same order. |
| §4 rule 9 (publish after commit) | Not applicable: no Timeline writes. |
| §1 (Capability and App Server boundary) | The leaf is the CLI process and its rutis root; it touches App Server business state only over protocol v3. Transport, Connection and Rpc gain only the `ui.*` built-ins and one notification; no Timeline fact. |
| ADR 002 D1 (scope follows the owner) | `UiHostPlugin` and every `UiLeaf` live in the daemon's process scope; the leaf's `Tui` lives in the leaf, the process the shell started on that terminal. |
| D2 (no wiring modules) | No adapter plugin: `UiLeaf` and `LeafNode` are each side's plugin whose life is the session. One-shot actions stay functions. |
| D3 (dependencies point to the core) | The leaf depends on the daemon's public protocol and on the rutis session contract; no daemon plugin knows `Tui`. |
| D4 (structural vs business configuration) | No loader rows; a leaf exists per `ui.open`. Terminal mode, workspace and input history remain the leaf invocation's own. |
| D5 (language-neutral contracts) | The business contract is the wire protocol, `ui.open`'s reply included; `ui/*` events will be serde values. |

## 10. Requirements raised to mechanism owners

None. One rutis limit is accepted rather than raised: the session catalog's
key (`catalog_key()`) is `#[doc(hidden)]`. rutis-bridge's catalog module
documents it as where a host provides its catalog, and Dim uses that
documented place only to isolate the daemon's catalog from the leaf.

## Appendix A. Implementation notes

- `run_cli` keeps the `tokio` runtime it builds; the rutis root lives on it. The TUI's blocking loop moves from the main thread to the `dim-tui` thread spawned in `Tui::apply` (as `rutis-agent`'s `TuiPlugin` does); the main thread blocks on the root until `Leave` resolves.
- `SessionHost` becomes the session thread inside `Tui`; `SessionPort` stays the interface between `ui` and the session thread, unchanged.
- `DaemonLink` takes `Options` by value and a `start: bool` (false only for `daemon stop`).
- Terminal restoration is registered as the first effect, before entering raw mode, so a failed load still restores the terminal.
- The new module sits at the top of `dim-cli`, beside `lib.rs`: it uses `client`, `session_host` and `ui::tui`, which the `xtask` layer rules allow (`session_host` must not use `ui`, and `ui` must not use the line client; neither is touched).
- Actual change: one module `crates/dim-cli/src/plugins.rs` (`Connection`, `DaemonLink`, `Tui`, and the `WebHold` used by `dimcode web`), `run_cli` rewritten to build and dispose the root, `SessionHost::start` taking the connection instead of connecting, and `dim-cli/Cargo.toml` gaining `rutis.workspace = true`. `/web` is a `web.toggle` call in `SessionHost` and no longer unloads `Tui`. Under `ui/` only the restore surface and the Web report are touched; no loader row or daemon plugin is added.
