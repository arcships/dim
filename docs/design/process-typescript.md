# TypeScript process rows and management methods

Status: Implemented for [#4](https://github.com/arcships/dim/issues/4), following
[#232](https://github.com/arcships/dim/pull/232). The public embedding input supports
process, [C4 Session](session-typescript.md) and [C5 branch](branch-typescript.md)
rows in one runtime. Foreign management contributions use the existing
[native management](appserver/management-methods.md) dispatch and call owner.

## 1. Scope and existing mechanisms

The App Server already assembles process plugins with rutis-loader, runs Node
rows, exports process services by name and dispatches native `ManagementMethods`
contributions. Its public TypeScript input places each configured row at process,
Session or branch scope. A foreign `managementMethods:<namespace>` declaration
is bound to a native management contribution inside its original row.

These host bindings use the existing runtime, loader and method
owners. Plugin business state and work stay in its package. There is no new
Capability, adapter row, registry, manifest, event, RPC endpoint or rutis protocol.
Package acquisition, editable installation and CLI installation remain
[PluginManager](plugins/PluginManager.md) / [#3](https://github.com/arcships/dim/issues/3)
work. The embedding caller supplies the rows explicitly, as it does for C4.

Source anchors: [`serve` / `Processes`](../../crates/dim-app-server/src/lib.rs),
[`Parts` / same-row binding](../../crates/dim-app-server/src/session_typescript.rs),
[`ManagementMethods` / `Owners`](../../crates/dim-app-server/src/methods.rs) and
the [shared method owner](../../crates/dim-agent-capabilities/src/methods.rs).

## 2. Host input and process placement

Extend the existing public `SessionTypeScriptConfig` with
`process_rows: Vec<serde_json::Value>`. Keep `ServerConfig.session_typescript`, its
runtime arguments and its existing `rows` member. An empty `process_rows` keeps
the current Session/branch installation. The name of the configuration type is
retained to avoid an unrelated rename. Rust struct-literal callers must add
`process_rows: Vec::new()`; update all repository examples and transport fixtures
together. This is a Rust source change, not a persisted or wire format change.

Both arrays use ordinary loader rows and the existing `RowSchema.provides`
snapshot. For example, a row in `process_rows` can declare:

```json
{
  "id": "example-process",
  "name": "/absolute/example/process.ts",
  "provides": {
    "managementMethods:example": { "describe": "async", "call": "async" }
  }
}
```

The placement rules are:

| Input | Placement and lifetime |
| --- | --- |
| `process_rows` | Ordinary top-level rows in the existing process assembly; one instance per host, independent of Session creation and closure. |
| `rows` | Existing C4/C5 classification and instanced Session/branch placement, unchanged. |

Use the same public Node runtime, resolver and declaration catalog for both
arrays. Keep its existing runtime name and Settings event forwarding. Do not
start another runtime for each package or reuse the private managed-provider
slot. A process row uses its original non-instanced factory, not a fabricated
`SessionScope`. The original row remains the owner of its foreign resources and
any native method contribution.

`session.event` is not a process-row observation contract. When an enabled
Session row declares `sessionEvents:*`, the shared runtime receives process-wide
emissions for all Sessions. Trusted process rows may technically subscribe too;
their incidental visibility depends on other installed rows and grants no
supported process API or isolation guarantee. C4's Session-id check is routing,
not a sandbox. P6/P7 process parts use declared `sessions` reads; keep Session
observation in the Session binding. No event filter or new subscription service
is added here.

This embedding input is trusted host configuration. It does not let a project
enable its own package or grant permissions, and introduces no host-local approval.

Validate the combined external configuration before assembly:

- Row ids are unique across both arrays and do not collide with host rows or
  generated module names. Rows of one module share the same declaration snapshot.
- Register process service names in the existing `ServiceCatalog` as shared,
  without an instance key. Reject duplicate process identities and collisions
  with native exports. Scoped identities retain their existing instance keys.
- A process row cannot inject Session/branch-only services or provide
  `toolContribution:*`, `sessionMethods:*` or `sessionEvents:*`. Session rows do
  not acquire process placement by declaring a management family.
- `managementMethods:<namespace>` has a nonempty, valid namespace and exactly
  the asynchronous `describe` / `call` modes. Other process-provided services
  retain the ordinary bridge method declarations; no host business parser is
  introduced for them.
- Reuse C4's comparison of the online module declaration with its structural
  `provides` snapshot, including its narrowly identified offline placeholder.
  A mismatch fails resolution of that row; names do not change at runtime.

Read `settings` source declarations from both arrays at the existing external
boundary, enforce one owner per source and strip that host-only metadata before
passing rows to the loader. A package declares a shared source on its process
row, rather than also declaring it on every Session part. Business updates
continue through Settings and its events; they do not reconcile loader rows.

### 2.1 Startup and availability

Configured `process_rows` are optional extensions under ADR 002 D3 / #34. Their
absence, Pending dependencies or Failed/Unresolved generation makes their own
services unavailable; those row states are not prerequisites for native host
startup. Merely enabling a package does not make its rows required infrastructure.
This isolates row availability after declaration discovery; it does not remove
the shared module-loading limit below.

Keep one initial reconcile in the existing assembly layer. Record the required
host row ids from the host's composition, before adding the extension rows.
`Processes::views` / `wait_active` use those required ids, rather than treating
every top-level Layer(0) row as required. Keep the loader, existing host rows,
shared Node runtime and `RuntimeRows` declaration publication in the required
wait, with their current deadline and error handling. This changes the host's
readiness selection; it neither satisfies nor removes an extension's declared
hard dependencies. Missing dependencies still leave that row Pending.

Initial `ReconcileReport.failures` also need the same distinction: a failure
whose typed `Failure.id` belongs to an extension row remains that row's
diagnostic, rather than a host startup error. Required-row failures remain fatal.
Malformed structure, duplicate identities and skipped rows (`report.issues`)
remain configuration errors; do not parse diagnostic strings to classify them.
The namespace inventory still names installed rows, so Rpc can distinguish an
unavailable owner from an unknown namespace using its existing row-status table.

Preserve the existing runtime gate: named Node factories inject `RuntimeRows`.
While the initial reconcile holds the loader operation, the rows remain Pending;
the existing `RuntimeRowsPlugin` refresh task obtains that operation afterwards,
refreshes declarations and then publishes `RuntimeRows`. Only then can extension
`apply` and management `describe` start. A row's own resolution failure is
reported in its status while the refresh continues for other rows. Shared
runtime/declaration infrastructure failure retains the existing C4 failure
boundary; runtime crashes are infrastructure failures under ADR 002 D3.

Known shared-gate limit: `loader.refresh` awaits Node's `describe_row`, including
module evaluation, with no per-module timeout. A module that never finishes
top-level evaluation, such as a top-level `await` on a network request, prevents
`RuntimeRows` publication. The host then reaches its existing startup deadline
even if native rows are ready. This also affects the current C4 binding; a hung
load is different from a completed resolution failure. Follow the
[author-guide module rule](../typescript-plugin-authoring.md#plugin-shape-and-lifetime):
module top levels do no business I/O or asynchronous business initialization.
Resource acquisition and work belong to the plugin's owned lifecycle. Record
this residual for shared-runtime design; no timeout, retry or rutis change is
proposed here.

This ordering matters because rutis-loader 0.12.1 reconcile still waits for
Loading rows: filtering `views` alone does not make an arbitrary reconcile
non-blocking. Keep the existing gate and verify a held extension `apply` after
declaration publication does not delay native startup. No new layer, detached
reconcile task, readiness phase or optional-dependency API is introduced.

### 2.2 Restart and owner reload

`RunningServer::restart` uses the same required-host selection as startup for
its final `processes.wait_active`. An unrelated optional row that is Pending or
Failed must not make `restart("Rpc")`, `restart("Agents")` or another allowed
restart fail after the required rows are Active. Preserve actual failures of
the requested required row, its required dependents and default-Agent setup.
This is the same assembly-owned predicate, not a second restart policy.

Keep the public `RESTARTABLE` allowlist. It does not accept arbitrary TypeScript
row ids. A process owner reloads with a declared dependency or the shared
runtime; structural declaration changes still require rebuilding the embedding
configuration. Exercise dependency reload through the existing loader handle in
the native fixture, not an invented public owner-restart endpoint. Direct
`Loader::restart` is a fixture control, not an expanded `RunningServer` API.

During owner reload, keep the existing per-namespace status, admission and
generation settlement rules. Rpc and Sessions do not acquire dependencies on
that owner; real declared dependents retain rutis' normal reload behavior.

## 3. Management declaration and dispatch

Derive `(namespace, original row id)` from each process row's
`managementMethods:<namespace>` entry and append it to the existing `Owners`
input. There is no separate namespace configuration. Retain disabled owners in
the known management namespace inventory, matching native longest-namespace
routing: a disabled child namespace is unknown and is never served by its parent.

Apply existing namespace grammar, reserved roots, built-in exclusions, duplicate
ownership and management/Session overlap validation at assembly. Do not accept
a process namespace that contains or lies inside a native management namespace,
or a namespace that would fail `ManagementMethods::provide`. Process rows may
still own nested namespaces relative to each other. Rpc continues to
validate that every configured namespace names a loader row. It neither injects
nor retains a foreign provider; each request looks up the native contribution.

The language-neutral payloads are the implemented subset of
[session-boundary A.1](contracts/session-boundary.md#a1-method-contributions):

| Method | Input | Output |
| --- | --- | --- |
| `describe()` | No arguments. | `MethodDecl[]`: `name`, `mutating`, `interruption` (`report` / `drain`), `progress`. |
| `call(call, cancelled, progress)` | `{callId, method, params}`, a by-reference cancellation `Promise<void>`, and a callback receiving one JSON value. | `{kind:"value", value?}` or `{code, appError, message}`. Omitted value means JSON `null`. |

`callId` is the binding's opaque correlation id. Preserve the client's params
without adding a Session id or inventing origin fields. Notifications, handoff,
paired-device management, a generic `callChannel` service and new discovery
endpoints are outside this subset. Unsupported declarations or replies fail
explicitly, as in C4; they are never accepted and then ignored.

The native binding lives in the original process row's apply:

1. Subscribe to the existing service-projection notification before applying
   the resolved runtime row. Register its cleanup immediately with `ctx.effect`.
2. Await this row's declared foreign projections, using uninstanced
   `host_key("managementMethods:<namespace>")`. Read only its own services;
   external dependencies remain declared. Row cancellation ends a pending bind.
3. Call `describe`, decode once at the bridge boundary and provide native
   `ManagementMethods` with its existing declaration validation and
   `MethodProvider`. Publish it before this row becomes Active.
4. Reuse C1's foreign method decoding and invocation in the shared method owner;
   do not copy a second DTO/parser/handler implementation into Rpc. Session and
   management contributions keep their distinct native keys and scope rules.

Use the current protected-local protocol-v3 entry admission, `Owners` routing,
`hello.features`, row-status errors and progress/reply delivery. No call may
switch to a new provider generation after admission. Declared application
errors preserve their code and `appError`; transport or malformed-reply errors
use a redacted binding failure rather than exposing runtime stacks.

## 4. Work ownership and cleanup

Register native call admission/drain cleanup after foreign row cleanup, as C4
does, so accepted calls settle before the foreign row is released. Provider or
admitting Rpc unload resolves the cancellation Promise for both interruption
modes. `report` replies `operation_interrupted` while its original work settles;
`drain` returns the original work's result. Both remain tracked until settlement.
End each Promise lease on ordinary completion as well. A client disconnect does
not cancel admitted work. Uncooperative JavaScript may delay unloading; no forced
success, timeout, rollback or replay is inferred.

Business background tasks and their effects belong to the process package.
Closing a Session does not dispose that package. Process shutdown follows the
existing host shutdown and loader disposal sequence; the binding adds no second
lifecycle manager or generic process quiescence service. This does not claim to
settle P6/P7 timer, report or native-writer lifetime acceptance.

## 5. Impact and verification

| Owner | Necessary implementation scope |
| --- | --- |
| App Server assembly / TypeScript binding | Add `process_rows`, validate combined declarations, use original process factories, assemble their rows, collect Settings sources and derive management owner pairs. Use the same required-host row selection for startup/report errors and restart completion (§2.1–2.2). |
| Shared method owner and native management owner | Reuse the existing foreign decoding/call boundary; provide `ManagementMethods` inside the original row, with the existing native admission and cleanup. |
| SDK types and author guide | Describe the supported process contribution using the existing method payload definitions; retain existing Session exports for callers. |
| Examples and transport fixtures | Update all configuration literals and add a real Node process-owner fixture through normal protected-local Rpc. |

Implementation acceptance uses public flows, not private helper assertions:
the optional-row cases below assume module declaration loading completes. They
do not claim isolation from the hung module-load residual in §2.1.

1. One configured process owner serves two Sessions, survives either closing,
   and exposes a declared management method through local v3 `hello` and Rpc.
   Existing C4 Session/branch rows still use the same runtime correctly.
2. Empty `process_rows` preserves current C4/C5 behavior. A disabled row is
   unknown; malformed/colliding structure is rejected. With a real missing
   declared dependency, native startup and Session calls succeed while the row
   stays Pending and its namespace answers `service_reloading`. A throwing
   `apply` or invalid declaration makes only that owner unavailable. Hold an
   extension's asynchronous `apply` after runtime declaration publication and
   verify native startup still completes; release it through its original
   effects. Required infrastructure failures still fail startup and clean up.
3. Use the fixture's existing loader handle to restart a process dependency that
   the TS owner declares. Check owner unload, reapply and management status over
   Rpc; make its reapply fail and then call `RunningServer::restart("Rpc")` and
   `restart("Agents")`. Both complete when required rows are Active despite the
   unrelated Failed row. Repeat with a Pending owner. Rpc, connections and
   unrelated Sessions keep their existing lifetimes except where the requested
   native restart normally reloads them. An arbitrary TS id remains rejected
   by `RunningServer::restart`. Namespace precedence and transport/version
   restrictions remain unchanged.
4. Exercise progress, a provider application error and an invalid reply through
   Rpc; verify admitted `report` and `drain` calls settle on original-owner and
   Rpc reload before the foreign cleanup finishes, including client disconnect.

Use disposable homes. Run `xtask check` / `graph` when assembly implementation
changes, the affected strict TypeScript and native Node flows, and the repository
merge checks for the implementation. Executable acceptance lives in the
[process transport flows](../../crates/dim-app-server/tests/transport_flow/process_typescript.rs);
existing C4/C5 flows cover the empty-process input.

This is not full P6/P7 readiness: §5.1 defines their accepted `scheduleJobs` /
`workflowRuns` binding; P6 live-policy/reuse and P7 definition-source/native-writer
contracts remain separate. A process service declaration does not authorize
undeclared `ctx.use`,
an optional lookup API or a hard dependency from every Session to its owner.
P7 A17 already uses the first actual `sessions.launch` reply's policy.

The accepted [native access contract](p6-p7-bindings.md) supplies generated
`sessionPermissions` for current branch policy and host defaults, and
`workflowFiles` from Settings for fixed project directories and the calling row’s
read-only packages. Both use the current Rust export generator. Native
`sessions.input` retains admitted work and drains it on the original row before
replacement. These bindings do not establish
P6/P7 business acceptance or full Ready.

<a id="51-proposed-p6p7-access-through-declared-feature-dependencies"></a>

### 5.1 P6/P7 access through declared feature dependencies

Status: the feature binding is accepted by the maintainer in
[the #237 architecture review](https://github.com/arcships/dim/pull/237#pullrequestreview-5584479437),
with the dependency and recovery constraints below. This replaces #35's per-call
owner replacement requirement. Full Scheduler/Workflow Ready remains separate.

Use the current rutis 0.12.1 row dependencies. The Scheduler Session tool row
module declares `scheduleJobs` in `definePlugin.inject`; the Workflow Session
tool module declares `workflowRuns`. This is the dependency description the
runtime resolver uses to gate the row; a loader option alone is not a replacement
for the module's declaration. Each uses its declared service through `ctx.use`
and calls its business operation from `execute`. Their process rows provide those ordinary
named services, alongside their own management contributions. No optional lookup,
new event contract, adapter row or rutis upgrade is required by this binding.

The dependent is the optional feature row, not Session assembly, ToolRegistry,
Loop or Rpc. An absent or Pending process owner leaves that feature row Pending;
native Session use follows the existing optional-row rules. Business Settings
edits still do not reload either row. A structural owner reload may retire its
real dependents, as ADR 002 D3 permits.

The process dependencies specified by [Scheduler §1](plugins/Scheduler.md#1-structure)
and [Workflow §1](plugins/Workflow.md#1-structure) share `sessions`, `settings` and
`pluginFiles`; Scheduler also injects `sessionPermissions`, and Workflow injects
`workflowFiles`. The stable `AgentDirectory` provides both `sessions` and
`sessionPermissions`, rather than the reloading Agents index. `workflowFiles`
belongs to the Settings generation the Workflow row already injects, so it adds
no independent reload source. `settings` and `pluginFiles` do not depend on
Provider or the Agents index. Provider and Agents-index reloads therefore do not reach these owners
through the old dependency chain in #35. Structural reloads of their declared
dependencies still propagate. The owner rows must keep this stable dependency
boundary: adding a frequently reloading dependency needs a new availability
decision. If an owner also injects a reloading service such
as Provider's `auxiliaryModel`, every Provider reload would retire that owner and
its dependent Session tool rows, bringing the availability cost below to every
existing Session.

**Availability after reload.** ToolRegistry follows contributed tools as their
providers activate and retire ([session boundary A.5](contracts/session-boundary.md#a5-tool-contribution), #241).
When an owner reload retires a tool row, its set is withdrawn; when the row's next
generation is active, existing and new Sessions are offered it again as a new
version from their next request. C4 still rejects calls through a handler whose
row has retired, and a request keeps the generation its prefix adopted. The
binding adds no hot rebinding of an issued call or automatic replay of a failed
operation.

A call rejected before invoking a retired foreign tool returns `is_error: true`
with this model-visible `content`:

```text
tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.
```

If the row unloads after the call has been issued, C4 ends the native wait without
proving that the JavaScript work stopped or had no effect. The error instead says:

```text
tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.
```

Session recovery does not undo accepted work. For example, check Scheduler job
state or Workflow run history before repeating a start; a fresh binding alone is
not proof that the earlier operation failed.

The real Scheduler and Workflow packages must also reject calls once their own
cleanup begins; the host's retained-generation guard does not replace that
package contract.

**Recovery.** No Session reload is needed: once the owner and the dependent tool row
are active again, the next request uses the current provider. Calls rejected or
interrupted during the reload are not replayed.

The actual Node flow
`node_process_typescript_owner_reload_keeps_native_session_work_running` exercises
that boundary through public Session calls: two Session tool rows inject one
process service; the process owner restarts while a native tool is held; a new
Session's tool reaches the replacement owner; the admitted native tool completes;
the retained old handler rejects another call; an existing Session's next request
reaches the replacement owner without a reload; and native tools continue in both
existing Sessions. The fixtures reject calls after their own cleanup begins,
as the existing retained-object contract requires. This is evidence for the
current shared-runtime binding, not for new cross-runtime or crash semantics.
The existing unavailable-owner flow also gives its Session tool row the declared
dependency: Pending, Failed, Unresolved and disabled owners leave the native
Session usable and omit that tool from the model's first request.

P6 live-policy capture/reuse, P7 definition readers and report settlement, and
the other feature-specific Ready decisions remain separate. No Permission,
Timeline, storage or persistence contract changes here.

### 5.2 Public pluginFiles binding (proposed)

`pluginFiles` is a shared named service with an implemented text/transaction API.
The current `serve` composition registers its `PluginFilesPlugin` only inside
the managed-plugin condition. A public process row that injects it therefore
has a Missing dependency, including with `product_home` configured; it stays
Pending under the existing rules and its management method reports
`service_reloading`. A catalog entry or SDK declaration does not install a provider.
This blocks Scheduler and Workflow readiness and their durable run-store checks.

Use the existing native provider and authorized product root:

| Host configuration | Proposed registration |
| --- | --- |
| Authorized `product_home` | Register the existing `PluginFilesPlugin(product_root)` once in the native process rows, outside the managed-plugin-only condition; availability does not depend on which process, Session or branch rows are configured |
| No authorized product root | Do not invent a root or fall back to the working directory; no provider is supplied |

This changes only native composition. Keep `pluginFiles.open`, the scope/lease
API, S10 outer v1, errors, FileStorage authority and provider lifetime unchanged.
No row field, SDK method, dependency registry, optional lookup or rutis/core
mechanism is added. The managed plugin keeps its existing binding; public rows
use their own ordinary runtime and declarations, without a managed-plugin slot.
The provider depends only on FileStorage, preserving §5.1's stable boundary.
Session and branch rows resolve this same shared service through the existing
catalog; a Session-only configuration must not require a dummy process row.

All trusted consumers use that same host-authorized root. A FileScope owns
operations and cleanup; it is not a per-package directory sandbox. Consumers
use their declared relative paths. The embedding host provisions their parent
directories before enabling them; the existing file API does not create missing
parents. Scheduler and Workflow use their specified `scheduler/` and `workflow/`
directories. Provisioning belongs to installation, not feature-specific App
Server branches or a new filesystem method. PluginManager #3 remains separate;
explicit embedding can supply the directories and rows today. If a parent is
not provisioned, the first `withLock` fails with the existing `io` result before
the callback can read or write. The consumer surfaces that storage failure and
starts no run effect; it does not create a Session or submit input without the
required durable transition.

Before claiming this binding implemented, use the actual Node bridge with an
isolated product home and no managed plugin: public process and Session-only
configurations become Active, open a scope, replace/read versioned text under
that root, and drain owned work before scope closure. Include an unprovisioned
parent: `io`, no new directory, and no run effect. Preserve the existing missing-root and optional
row behavior, and verify the managed configuration registers only one provider.
Then run Workflow's persisted-attempt, failed-replacement and host-restart
acceptance under this public binding. Neither feature is Ready merely because
the API exists. The binding proposal requires architecture/storage review before
production implementation.

## 6. Rule check

| Rule | Application |
| --- | --- |
| §4.1 Commands/results | No Timeline mutation is added. Process business methods retain their owner's typed writes; branch effects use existing Sessions/input contracts. |
| §4.2 Handover | Existing method admission and branch-input handovers remain unchanged; a lost reply does not undo work. |
| §4.3 Before/entry checks | No before callback. Host checks structural declarations; method owners validate business params. |
| §4.4 Core has no business meaning | Assembly and Rpc read service identities and namespaces, without Scheduler/Workflow cases. |
| §4.5 Batches | No event or batch change. |
| §4.6 Model context | Management bindings introduce no context path; inputs and tool results retain their existing owners. |
| §4.7 Public access | Supported process behavior uses declared dependencies and the original row's projections. Shared `session.event` visibility is incidental runtime behavior, not a process observation contract or sandbox (§2). No private plugin state is accessed. |
| §4.8 Infrastructure | #4 completes host bindings and makes the host readiness selection obey D3 for configured optional rows, including startup report failures and restart completion (§2.1–2.2). §5.2 proposes placing the existing storage provider in public composition; required-row waiting, rutis and the runtime gate are unchanged. |
| §4.9 Durable facts | Assembly metadata and process call bookkeeping do not enter Timeline. |
| §1 Scope/App Server | Process parts are loader rows, not Capabilities; App Server owns assembly/wire binding, packages own all feature work. |
| ADR 002 D1 | Process rows outlive Sessions; existing Session/branch rows retain their own lifetimes. |
| ADR 002 D2 | Bind in the original row; no adapter or management plugin. |
| ADR 002 D3 | Rpc resolves providers per call. Host startup and restart readiness exclude optional extension rows while those rows retain their own hard dependencies and normal reload behavior (§2.1–2.2). A hung top-level module load remains a shared-gate startup limit (§2.1). |
| ADR 002 D4 | Structural declarations stay in rows; Settings documents remain business data and do not reload rows. |
| ADR 002 D5 | Named `managementMethods:<namespace>` implements the existing neutral payloads; Rust `ManagementMethods` is its native binding. |

## 7. Requirements raised to mechanism owners

| Item | Content |
| --- | --- |
| Contract served | ADR 002 D5's process `managementMethods` contract needs ordinary process placement and projection from its named foreign service to native management dispatch, owned by the original row. |
| Existing mechanisms considered | Reuse the public C4 runtime, `RuntimeResolver`, `ServiceCatalog`, original-row projection, `Owners` and `MethodProvider`. The same assembly-owned required-row selection covers startup failures/readiness and restart. The existing `RuntimeRows` gate orders initial reconcile and extension apply without another task. Session factories have the wrong lifetime for process owners; events cannot install an absent row or contribute its management namespace. |
| Generic binding | Explicit process rows, same-row named management projection, and required-host selection for startup and restart within App Server assembly. No new core service, facade, rutis requirement, loader API or public restart endpoint. |
| Ordering and lifecycle impact | No Timeline sequence, batch or event change. Keep one initial reconcile, the existing runtime declaration gate and required-row deadline. Optional row failures stay local at startup and restart; native management admission and provider report/drain settlement remain unchanged. |
| Implementation owner / issue | zerob13, App Server assembly and method binding under #4. Architecture review by the existing assembly/management maintainers; this does not assign new work to S2 or rutis. |
| Public storage placement | Public rows declare the existing pluginFiles service, but serve registers its provider only in managed composition. Reuse PluginFilesPlugin and the authorized product root under §5.2; do not use a Session factory, private plugin slot or new filesystem API. This is App Server composition work for #70/#71, owned by zerob13 with architecture/storage review. No Timeline ordering, lifecycle protocol or mechanism-owner extension is requested. |
