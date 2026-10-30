# Management methods

Status: process-owner contributions implemented. This page describes the implemented
subset of [ADR 002](../../adr/002-plugin-scope.md) and the
[methods contract](../contracts/session-boundary.md).
Management methods belong to process owners, not a Management plugin. There is
no per-method level; entry admission determines whether they are served.
The earlier stable-port and Management-plugin proposals are withdrawn.

[Provider management](provider-management.md) is a consumer proposal (#329) for
additional owner methods through this existing binding. It uses rereads and CAS,
and requires no new dispatch, connection notification or managed host-API surface.

[ADR 002 D5](../../adr/002-plugin-scope.md#d5-contracts-are-language-neutral)
requires language-neutral contracts. The process has one management table, the
process service `managementMethods`, which App Server's `ManagementMethods` row
provides. A native owner depends on it and calls
`register(ctx, namespace, declarations, handler)`; a foreign process row injects
`managementMethods` and calls `register(namespace, declarations, call)`, which
returns a Promise ([methods contract A.1](../contracts/session-boundary.md#a1-method-contributions)).

The [TypeScript process binding](../process-typescript.md) registers from inside
the original process row and reuses this dispatch and settlement. Assembly
records no namespace.

## Responsibility and assembly

Each process owner registers its namespaces with `managementMethods` during
`apply`. The registration belongs to the owner and is withdrawn when it unloads;
the owner's ordinary rutis lifetime owns the calls it accepts. Rpc depends on the
table, not on any owner ([methods contract §3.2](../contracts/session-boundary.md#32-management-method)).
The table has no dependencies, so it reloads neither with Rpc nor with an owner.
The owners register `account`, `account.codex`, `account.grok` and `provider`
with a product home; `settings` (SettingsMethods) wherever Settings is assembled;
and `mcp`, `skills` and `permission` with local execution. Adding an owner adds
its row, whose `apply` registers its namespace; nothing else in the assembly
changes. On each call Rpc reads the table for the namespace that holds the name
and dispatches to its provider; it reads the status of the row that registered
the namespace only when that namespace is withdrawn (below). rutis is an external
dependency: this uses its effects and the loader's row status as they are, and
raises no requirement to it
([principles §4 rule 8](../../architecture/rust-agent/principles.md)). No
contributor injects Rpc or Connection; neither contains contributor-specific
dispatch. Adding a method changes its owner's declaration, parsing and
implementation.

Codex, Grok, Provider and Mcp register their namespaces directly, without
adapter rows; ManagedProviderMethods, which depends on the process-owned
managed-provider service, owns `account`. Settings supplies generic
BusinessDocument persistence and the declared sources of
[the settings service](settings-service.md), not approvals, a method contribution or a configuration
namespace; SettingsMethods, which depends on it, owns `settings.read`/`settings.update`.
MCP owns its configuration types in `mcp/config.rs`; the Skills owner supplies
the existing `SkillSettingsHost`. App Server's SkillsCatalog mounts the reusable catalog
and publishes both catalog and management interfaces in one plugin, with the
host's `HostPlatform`/`SettingsService` dependencies and the catalog's original effects;
the Skill settings adapter stays private to that owner. The capabilities crate does not depend on
App Server's wire protocol. Management request and response DTOs belong to their
business owners; `protocol` retains the envelope and built-in Session contracts.
MCP management and built-in Session methods share the MCP owner's error mapping.

The table checks each registration: it refuses a namespace outside the name
grammar, in the `hello`, `daemon`, `agent`, `approval` or `session` roots, or
with a prefix equal to a built-in method ("Invalid management namespace"), and
declarations that name a built-in method ("Invalid management contribution"), so
contributed methods cannot impersonate Session traffic. Declarations must hold
at least one method, each unique and inside the namespace. A second registration
of one namespace fails (`<namespace> is registered already`). A refused
registration fails the owner that made it. A name belongs to the longest
registered namespace that prefixes or equals it: a parent's method inside a
registered child namespace is neither dispatched nor listed, and the child's
namespace keeps holding it after it is withdrawn. A child that never registered
holds nothing, so the parent serves that method. Valid nested namespaces such as
`account.codex` remain independent from the parent `account` namespace.

## Dispatch and lifecycle

Entry admission precedes routing for built-in and contributed methods alike:
a protected profile answers every call except `hello` and `daemon.ping` from a
non-local transport with `local_transport_required`, as before contributions
([methods contract A.3](../contracts/session-boundary.md#a3-entries-and-export)). Local v3 then dispatches management contributions; v1/v2 do not.
A name no management namespace holds, registered or withdrawn, whose params
object has a `sessionId` field is a Session method candidate instead, dispatched
through the Session its `sessionId` names
([methods contract A.1](../contracts/session-boundary.md#a1-method-contributions)).
No assembly check keeps Session and management namespaces apart: a management
namespace takes the names it holds, and a Session namespace inside one never
receives them.
A name no namespace ever held, a name its provider does not declare, and a
withdrawn namespace whose row is disabled or absent, or Active without
registering it again, return `unknown_method`. A withdrawn namespace whose row is
not running and has not failed (not started yet, Pending, Loading, or between
generations) returns the retryable `service_reloading`; one whose row cannot run
again until it is fixed or restarted (Failed, Unresolved, Stopped or Disposed)
returns the non-retryable `service_unavailable`. The table remembers the row
that registered each withdrawn namespace, and Rpc reads its status from the
loader (contract §3.2). An owner that is still Pending, or that failed before it
registered, holds no namespace, so its names are `unknown_method`. A withdrawn
Session namespace answers the same way from the row that registered it, read as
that row's copy in the Session's instance: Rpc finds the copy whose innermost
instance is the Session's (`Session::instance`) among the loader's entries. A
withdrawn Session namespace whose copy is Active (a child plugin of the row
reloads), or that has no copy (a Rust Session part between generations), returns
`service_reloading`, as does a Session that is reloading. A name outside
the declaration grammar (dot-separated
lowercase segments, at most 128 bytes) is unknown without any lookup, so a
caller's name bounds the lookups made under the handler lock. Error codes at the
edges are unchanged: a call before `hello` is `-32600` and one after shutdown has
begun is `-32603`. A name no namespace holds is `-32601` when its params carry no
`sessionId`; with one, Rpc reads it first (a `sessionId` that is not a string is
`-32602`, one that names no Session `session_closed`) and answers from that
Session. Only the owner of a declared method rejects its other params (`-32602`).
The proposed `ConnectionEntry {name, deviceId}` and paired-device routing remain
RemoteGateway work; its connections must never be served management methods.
`hello.features` lists the methods of the namespaces registered at that `hello`.
A namespace registered later appears in the next `hello`. The withdrawn
`management.methods` endpoint is not provided. Existing Session and daemon
methods keep their built-in contracts.

How Rpc answers a local v3 call whose name a management namespace holds, or a
Session method whose namespace is withdrawn in its Session, and what the client
does with each reply (contract §3.2, A.1, Appendix C):

```text
owner of the name in the table?
  none: no namespace ever held it --> -32601 unknown_method   stop: nothing serves the name
  registered -----------------------> provider answers
                                      (a name it does not declare: unknown_method)
  withdrawn
        |
        v
status of the row that registered it   reply                         client
(loader.get) or that row's copy
------------------------------------   ---------------------------   ----------------------------------
owner row absent, or row Disabled      -32601 unknown_method         stop: nothing serves the name
Session part has no copy in the        -32000 service_reloading      retry: the part or the Session
  Session                                                              is coming back
Inactive: not started yet,             -32000 service_reloading      retry: the owner is coming back
  or between generations
Running: Pending, Loading, Unloading   -32000 service_reloading      retry: the owner is coming back
Running: Active                        registered again: the provider answers; else management
                                       -32601 unknown_method, Session -32000 service_reloading
Running: Failed or Disposed            -32000 service_unavailable    stop: retry after a fix or restart
Unresolved                             -32000 service_unavailable    stop: retry after a fix or restart
Stopped                                -32000 service_unavailable    stop: retry after a fix or restart
a status the loader adds later         -32000 service_unavailable    stop: retry after a fix or restart
```

`loader.get(row)` reads an owner row itself, and the loader reports Stopped
only for a copy inside an instance, so Stopped reaches Rpc only from a Session
row's copy that disposed itself. Two owners whose namespace is withdrawn
answer `service_reloading` although they will not come back without a fix
([#124](https://github.com/arcships/dim/issues/124)): one that is Pending on a
dependency that failed, and one that disposed itself. The process loader has no
editable layer in which to disable the second one's row, so once the loader
drops its fiber, the row reads Inactive. Both stay as they are until a shipped
configuration can reach them ([below](#owners-that-wait-without-end)).

Rpc admits contributed calls under the same handler lock as built-in commands
and tracks reply delivery in its command tracker. A request id is the client's
again once its reply is sent, even while an interrupted operation settles. The
operation belongs to its provider: the provider admits it under its own gate and
tracks it until it settles. Either unload, the provider's or the calling Rpc load's, signals the
handler. A `Report` call then replies `operation_interrupted`, stating that the
operation may already have committed and the current state must be read before
retrying; a `Drain` call replies with its own result. A provider's unload waits
for every call it accepted, so `Drain` calls hold it; this is the owner's own
effect within rutis' existing lifecycle. Rpc's unload
waits for the replies it owes and for each interrupted operation to settle, so the
next load reads its outcome. Once Rpc's own load is unloading it admits no
management call or `hello`; they return `service_reloading` and the next load
serves them. A caller's disconnect cancels nothing.
This reproduces the behaviour before contributions
([methods contract A.4](../contracts/session-boundary.md#a4-migration-and-compatibility)). Provider and
account methods use `Drain`; MCP and Skills use `Report`. Settings retains its
accepted-write contract.

An owner that is disabled, Pending, Failed or reloading affects only its own
namespace; Rpc and open connections are unaffected. Once its namespace is
withdrawn, its methods return `service_reloading` while its row is Pending, not
started yet, or between generations, as before contributions when Rpc reloaded
with it (contract A.4); `service_unavailable` while it is Failed, Unresolved or
Disposed; and `unknown_method` once it is disabled. Before it first registers,
its methods return `unknown_method`. Rpc and Agents depend on no owner, directly or
through each other: `session.create`, `session.model.update` and Collaboration
member creation read Provider and the `ModelSelectionHost` per call and return a
retryable error while it is not active. A transport flow asserts that no
management owner is among Rpc's transitive dependencies. Other execution
dependencies may still reload affected consumers and fail in-flight turns; this
is the accepted cost of rare structural changes. Business configuration edits update the owner's source and
publish invalidation without reloading the owner or Rpc. MCP connections remain
Session-owned. There is no host-local approval
([ADR 003](../../adr/003-approval-scope.md)); a project entry runs only after the
user enables it through `mcp.set_enabled`, which records the choice in MCP's
user-local business document; see ADR 004.

## Owners that wait without end

Status: decided for [#124](https://github.com/arcships/dim/issues/124): no
change until a reopen condition below holds. Checked against main at
rutis and rutis-loader 0.16.0.

Rpc reads only the row that registered a withdrawn namespace
([Dispatch and lifecycle](#dispatch-and-lifecycle)). Three states of that row
read as "coming back" although nothing brings them back:

```text
client -- workflow.list --> Rpc: namespace withdrawn; loader.get(registering row)
                              |
  owner Running(Pending) -----+--> service_reloading    (retry)
    inject X: Missing             but X's row is Failed: the owner waits for ever
  owner Running(Pending) -----+--> service_reloading    (retry)
    its Node runtime is gone      but the runtime row still reads Active
  owner Inactive -------------+--> service_reloading    (retry)
    after SelfDisposed            but no layer can disable the row: it stays Inactive
```

### Where each case can occur

Each case answers `service_reloading` only after the owner registered its
namespace and it was withdrawn; an owner that never registered answers
`unknown_method`.

| Case | Native process owner | TypeScript process owner ([process rows](../process-typescript.md)) | Shipped configuration today |
| --- | --- | --- | --- |
| Pending on a dependency whose row is Failed, Unresolved or Disabled | No. A required host row that fails during startup fails startup (`lib.rs`, required failures); after startup only `RunningServer::restart` can fail one, it has no caller in this repository, and its allow-list (`Rpc`, `Connection`, `Transport`, `Agents`, `Mcp`) holds no provider of an owner dependency | Yes, through the embedding input: an extension row's failure is its own diagnostic and does not fail startup (process rows §2.1), so an owner that injects another process row's service stays Pending when that row fails | None. `dimcode` configures no process rows; the planned Scheduler and Workflow owners inject only `sessions`, `settings`, `pluginFiles` and, respectively, `sessionPermissions` ([Scheduler §1](../plugins/Scheduler.md#1-structure)) and `workflowFiles` ([Workflow §1.3](../plugins/Workflow.md#13-concrete-package-rows)), all provided by required host rows that cannot be restarted (process rows §5.1) |
| Pending on a service no row provides | No: assembly wires every native dependency | Yes, once the owner has registered and reloads; before that its names are `unknown_method`, as process rows acceptance 2 asserts | None, as above |
| Node runtime lost after startup | — | Yes: when the Node process exits, rutis-bridge 0.16.0 `watch_exit` (`runtime/plugin.rs`) records `RuntimeState::Down` and withdraws the runtime service, but the local runtime wrapper has already returned `Effect::Done` (`runtime/local.rs`), so its row stays `Running(Active)` while every owner on that runtime goes back to `Running(Pending)`. A probe with the real Workflow row from [#306](https://github.com/arcships/dim/pull/306) and `kill -KILL` on the runtime confirmed it ([review](https://github.com/arcships/dim/pull/247#discussion_r4344393184)) | Reachable by an explicitly installed optional package such as Workflow (#306, open); not by `dimcode`'s default configuration. Accepted, see below |
| Disposed itself, row Inactive | No owner disposes itself | Yes: a JavaScript row whose plugin ends disposes its fiber (rutis-bridge `on_row_ended`, called from rutis-loader `runtime.rs`), and the process loader has no editable layer, so `SelfDisposed` carries the edit error | None, as above |
| Session method whose part failed | — | — | Handled by the row-copy status ([#207](https://github.com/arcships/dim/issues/207), contract A.1) |

No client here retries on `service_reloading`: `dimcode` returns the error
(`session.rs`). The harm of the misleading reply is therefore a client that
retries for ever, and there is no such client and no shipped configuration
that reaches the dependency or self-disposal cases. Rpc keeps its current
answers; adding a dependency walk now would be a defense without a trigger.

Runtime loss is a different kind of case: a shipped optional package already
reaches it, and nothing restarts the runtime. It is accepted as an
infrastructure failure under [ADR 002 D3](../../adr/002-plugin-scope.md#d3-dependencies-follow-scope-and-point-from-extensions-to-the-core)
("a runtime crash is an infrastructure failure ... nothing is designed around
it") and [Session TypeScript: Lifetime and failure](../session-typescript.md#lifetime-and-failure)
("no special runtime-crash recovery mechanism is added"). Until the host
restarts, the owner's namespace answers `service_reloading`. Neither this
decision nor the walk below covers it: the owner's dependency on its runtime is
implicit, outside `RowSchema.provides`, so it is not in the walk's table, and
the runtime row reads Active, so no provider row reads Failed. Covering it
would need a separate, explicit decision that reverses D3 for runtime loss,
owned by the runtime lane (eric8810): for example the runtime row leaving
Active when rutis-bridge reports `RuntimeState::Down`, after which the existing
row-status answers apply. That decision is not made here, and nothing is
raised to rutis.

### Reopen conditions

Revisit when any of these becomes true:

1. A shipped configuration (`dimcode`, package installation, or a first-party
   plugin) runs a management owner that injects a service of a row that can be
   Failed, Unresolved or Disabled while the host runs: a TypeScript process
   owner that injects another process row's service, a production caller of
   `RunningServer::restart` whose allow-list includes a provider of an owner
   dependency, or a process loader with an editable layer or configuration
   reload.
2. A shipped TypeScript process owner can end itself.
3. A client in this repository retries on `service_reloading`.

### What Rpc does when reopened

Only the trigger that exists is handled; native rows stay as they are.

Pending on a failed row (condition 1 or 3). On the error path only, when the
owner row is `Running(Pending)`:

```text
owner Running(Pending)
  fiber = loader.get(owner).plugin; its injects in Ctx::diagnostics() that are not Ready
  each Missing key -> provider row from the provider table (below)
    |- not in the table ---------------------------------> unchanged (service_reloading)
    |- row Failed | Disposed | Unresolved | Disabled -----> service_unavailable,
    |                                                       message names that row
    |- row Running(Pending) -----------------------------> repeat for it; a row
    |                                                       already on the path stops
    `- anything else ------------------------------------> service_reloading
```

- The table maps a service key to the process row that provides it. Assembly
  already holds both halves: each TypeScript process row's `RowSchema.provides`
  snapshot, which the resolver refuses when it differs from the runtime's
  description, and the `ServiceCatalog` that registers those names
  (`ServiceCatalog::key`). It is built in assembly and passed to Rpc with its
  configuration. Native rows are not in it, so no `provides` list is written by
  hand and none can drift.
- Disabled counts as blocked only because the process loader has no editable
  layer; if one is added (condition 1), Disabled returns to "coming back".
- A blocked verdict always comes from `loader.get`; diagnostics, best effort,
  only say which keys to look up. Dispatch to an active provider is unchanged.
- No new error code or field. Today `service_unavailable` carries a fixed
  generic message ("Service is unavailable until it is fixed or restarted",
  `unavailable()` in `methods.rs`) and receives no row. The reopened design
  changes how that message is built so that, for this case, it names the
  blocking row; the code and its non-retryable meaning stay the same.

A service no row provides, and a self-disposed owner, stay `service_reloading`
until their own trigger ships (conditions 1 and 2). The first needs a decision
that no provider will appear, which rutis does not report (a running plugin may
`provide_as` at any time); the second needs Rpc to keep `SelfDisposed` ids from
the loader's events in state that outlives Rpc's own reload. Neither is designed
before it can occur.

### Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (command events) | No event; Rpc's reply is a read of loader status |
| §4 rule 2 (handover) | Unchanged; admission and call ownership are as above |
| §4 rule 3 (before callbacks) | None |
| §4 rule 4 (no business meaning in core) | The reopened design reads only generic row status and diagnostics; no namespace is special-cased |
| §4 rule 5 (batch) | No batch |
| §4 rule 6 (model context) | Unchanged |
| §4 rule 7 (public events and services) | Rpc reads loader status and diagnostics, which assembly already exposes to it; it reads no owner's internal state |
| §4 rule 8 (infrastructure unchanged) | Unchanged; nothing raised (below) |
| §4 rule 9 (Timeline facts) | Nothing enters the Timeline |
| §1 (App Server boundary) | Reply selection stays in Rpc |
| ADR 002 D1 | Process scope, in Rpc |
| ADR 002 D2 | No new plugin or wiring module; the provider table is an input Rpc receives from assembly |
| ADR 002 D3 | Rpc still depends on no owner; runtime loss stays an infrastructure failure with nothing designed around it |
| ADR 002 D4 | No business configuration |
| ADR 002 D5 | The wire contract is unchanged: same codes and retry meaning |
| ADR 003 | Not applicable: no agent action |

### Requirements raised to mechanism owners

None. rutis and rutis-loader are external dependencies
([principles §4 rule 8](../../architecture/rust-agent/principles.md)); the
reopened design uses their existing loader status, `Ctx::diagnostics` and
`ServiceCatalog`. Listing a service's providers stays withdrawn
([contract §9.3](../contracts/session-boundary.md#93-listing-the-active-providers-of-a-process-scoped-service));
if rutis adds it, Rpc uses it in place of the table. Runtime loss is not a
requirement on rutis either: rutis-bridge already reports `RuntimeState::Down`,
and whether dim acts on it is the runtime lane's D3 decision, not made here.

## Notifications

A method that declares `progress` sends progress on its call through the
handler's `Progress`; Rpc delivers it as `management.progress {id, method, data}`
to the calling connection only, before the reply, and drops it after a
disconnect ([A.2](../contracts/session-boundary.md#a2-call-channel)). Progress
is best effort: a connection queues at most 256 progress frames and 4 MiB of
them, beside the 1024 frames and 32 MiB of replies and notifications; it
drops a notice that does not fit, so one over 4 MiB never arrives, and stays
open. Management
and Session methods share this path; no management method declares `progress`
today. Connection-bound notifications belong to the contract's
`callChannel.notify`, which is not implemented. Settings source watches invalidate business consumers
after successful persistence; they are not loader events or management
notifications. Persistence does not certify runtime adoption or generation
settlement.

## Limits and verification

RemoteGateway, rutis-link export, connection-bound CallChannel notifications and
shutdown reply handoff are not implemented in this subset. The Session method
table (`SessionMethods`) shares its `MethodTable`, declarations, handler, errors
and call admission, which live in dim-agent-capabilities `methods`
([methods contract Appendix F](../contracts/session-boundary.md#appendix-f-rust-binding)). There is no new settings authority, loader or authorization system.

`managementMethods.register` is the D5 binding: native and foreign owners
register through the same table, with the same owner, entry restrictions and
reload/drain guarantees, without an adapter plugin or a second registry.
The revision's Timeline-driven input withdrawal and peer-node offline behavior
belong to the Session event contract and rutis link work; neither is implemented here.

Method namespaces are not discovered from declarations: each owner registers its
own, from any language, and assembly lists none. There is no generic credential
service; each owner handles its own secrets. D4 keeps business documents in
their source, not editable loader rows; see [ADR 004](../../adr/004-loader-plugin-management.md).

Transport flows cover actual owner methods, hello features, an owner reload
that keeps the same connection, an Rpc reload, local/remote and protocol-version
boundaries, business source CAS, persistence failures and invalidation; every
flow assembles the real owner rows, which register their own namespaces. Unit
flows on a real loader cover a provider reload that interrupts `Report` calls
while an accepted `Drain` call holds it and answers `service_reloading` between
generations, the calling load's unload, owners that never registered (Pending,
failed before registering, Unresolved, disabled or unconfigured) answering
`unknown_method` while other namespaces and the listing stay intact, a
registered owner whose next generation fails answering `service_unavailable`,
refused namespaces (reserved roots and built-ins), and nested ownership with a
registered child or one that never registered.
Session method flows (`transport_flow/session_methods.rs`) register a namespace
from a test part and cover the namespace checks at registration, a Session
namespace inside a management one, dispatch through the Session, the routing
errors, a provider reload, Session close and restore, `session.methods`,
per-call progress, and progress dropped for a caller that does not read while
its reply still arrives. TypeScript Session part flows
(`transport_flow/session_typescript.rs`) cover the namespaces of Pending and
Failed parts that never registered answering `unknown_method`, a reloading
row's namespace answering `service_reloading`, and refused registrations.
