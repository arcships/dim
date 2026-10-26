# TypeScript process rows and management methods

Status: Implemented for [#4](https://github.com/arcships/dim/issues/4), following
[#232](https://github.com/arcships/dim/pull/232). The public embedding input supports
process, [C4 Session](session-typescript.md) and [C5 branch](branch-typescript.md)
rows in one runtime. Foreign management namespaces register with the same
[management table](appserver/management-methods.md) and call owner as native ones.

## 1. Scope and existing mechanisms

The App Server already assembles process plugins with rutis-loader, runs Node
rows, exports process services by name and dispatches the namespaces process
parts register with its `managementMethods` table. Its public TypeScript input
places each configured row at process, Session or branch scope. A foreign process
row injects `managementMethods` and registers its namespaces from its own apply,
as a native owner does.

These host bindings use the existing runtime, loader and method
owners. Plugin business state and work stay in its package. There is no new
Capability, adapter row, registry, manifest, event, RPC endpoint or rutis protocol.
Package acquisition, editable installation and CLI installation remain
[PluginManager](plugins/PluginManager.md) / [#3](https://github.com/arcships/dim/issues/3)
work. The embedding caller supplies the rows explicitly, as it does for C4.

Source anchors: [`serve` / `Processes`](../../crates/dim-app-server/src/lib.rs),
[`Parts` / same-row binding](../../crates/dim-app-server/src/session_typescript.rs),
[`ManagementMethods` / `management_provider`](../../crates/dim-app-server/src/methods.rs) and
the [shared method owner](../../crates/dim-agent-capabilities/src/methods.rs).

## 2. Host input and process placement

Process owners are rows of the existing public `SessionTypeScriptConfig.rows` without a
`group`. Every row of another language is such a row: its plugin registers its Session and branch
parts with `partRegistry` and lists the names they provide in `instanceNames`
([How Dim uses rutis §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)). The name of the configuration
type is retained to avoid an unrelated rename. This is a Rust source change, not a persisted or
wire format change.

All rows use ordinary loader rows and the existing `RowSchema.provides`
snapshot of the services they provide by name. For example, a process row that
registers a management namespace and provides no service declares:

```json
{
  "id": "example-process",
  "name": "/absolute/example/process.ts",
  "inject": ["managementMethods"],
  "provides": {}
}
```

The placement rules are:

| Input | Placement and lifetime |
| --- | --- |
| Every row (a `group` is rejected) | Ordinary top-level rows in the existing process assembly; one instance per host, independent of Session creation and closure. |

Use the same public Node runtime, resolver and declaration catalog for all
rows. Keep its existing runtime name. Do not
start another runtime for each package or reuse the private managed-provider
slot. A process row uses its original non-instanced factory, not a fabricated
Session instance. The original row remains the owner of its foreign resources and
of the namespaces it registers.

`session/{sessionId}` is not a process-row observation contract. rutis has one
bus per root, so trusted process rows can technically listen to any Session's
channel; that grants no supported process API or isolation guarantee. The
channel name is routing, not a sandbox. P6/P7 process parts use declared
`sessions` reads; keep Session observation in the Session binding. No event
filter or new subscription service is added here.

This embedding input is trusted host configuration. It does not let a project
enable its own package or grant permissions, and introduces no host-local approval.

Validate the combined external configuration before assembly:

- Row ids are unique across all rows and do not collide with host rows or
  generated module names. Rows of one module share the same declaration snapshot.
- Register a process row's provided names that the `ServiceCatalog` does not hold yet as shared,
  without an instance key; a row's `instanceNames` get their group's instance key. A part that names a service of a group it is not inside fails, by
  rutis; a second
  provider of a name fails with rutis's duplicate-provider error. `toolRegistry` and
  `sessionMethods` are Session services, so only Session parts register tools and Session
  methods; process rows register management namespaces with `managementMethods`.
- Process-provided services retain the ordinary bridge method declarations; no
  host business parser is introduced for them. Management namespaces are checked
  when a row registers them (§3), not here.
- Reuse C4's comparison of the online module declaration with its structural
  `provides` snapshot, including its narrowly identified offline placeholder.
  A mismatch fails resolution of that row; names do not change at runtime.

Read `settings` source declarations from both arrays at the existing external
boundary, enforce one owner per source and strip that host-only metadata before
passing rows to the loader. A package declares a shared source on its process
row, rather than also declaring it on every Session part. Business updates
continue through Settings and its events; they do not reconcile loader rows.

### 2.1 Startup and availability

Configured process rows are optional extensions under ADR 002 D3 / #34. Their
absence, Pending dependencies or Failed/Unresolved generation makes their own
services unavailable; those row states are not prerequisites for native host
startup. Merely enabling a package does not make its rows required infrastructure.
This isolates row availability after declaration discovery; it does not remove
the shared module-loading limit below.

The same holds for the Session and branch parts such a row registers: `serve`
can return while the row's `apply` has not registered them yet. A Session that
opens before the registration does not wait for it; it mounts the part after it
is published ([ADR 006 D3](../adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it),
Publication), so the
part's tools are offered from the first request after it is mounted. A Session
opened once the row is Active has them in its first request.

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
A namespace a row registered keeps naming that row after it is withdrawn, so
Rpc can tell an unavailable owner from an unknown namespace using its existing
row-status table. A row that stays Pending or fails before it registers holds no
namespace, so its names are `unknown_method`.

Preserve the existing runtime gate: named Node factories inject `RuntimeRows`.
While the initial reconcile holds the loader operation, the rows remain Pending;
the existing `RuntimeRowsPlugin` refresh task obtains that operation afterwards,
refreshes declarations and then publishes `RuntimeRows`. Only then can extension
`apply`, and with it management registration, start. A row's own resolution failure is
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

This ordering matters because rutis-loader 0.16.0 reconcile still waits for
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

## 3. Management registration and dispatch

A process row injects `managementMethods` and, from its `apply`, calls
`await ctx.use<ManagementMethods>('managementMethods').register(namespace, declarations, call)`
once per namespace. The registration belongs to the calling row and is withdrawn
when that row unloads. There is no separate namespace configuration, and the
host derives no namespace from the row's declaration.

The management table checks each registration as it checks a native owner's
([management methods](appserver/management-methods.md)): the name grammar, at
least one method, each unique and inside the namespace; no reserved root
(`hello`, `daemon`, `agent`, `approval`, `session`) and no prefix equal to an
exact built-in (`Invalid management namespace`); no declaration equal to a
built-in (`Invalid management contribution`); and no namespace registered
already (`<namespace> is registered already`). The row sees a refusal as a
rejected Promise; a row that lets it propagate from `apply` fails, and only its
own namespaces are affected. Rows may own namespaces nested in each other's or
a native owner's: a name belongs to the longest namespace in the table that
holds it, and a withdrawn namespace keeps holding its names, so its parent never
answers them. Rpc neither injects nor retains a foreign provider; each request
reads the table.

The language-neutral payloads are the implemented subset of
[session-boundary A.1](contracts/session-boundary.md#a1-method-contributions):

| Function | Input | Output |
| --- | --- | --- |
| `register(namespace, declarations, call)` | A namespace; `MethodDecl[]`: `name`, `mutating`, `interruption` (`report` / `drain`), `progress`, each required; the row's `call` function. | A Promise that resolves once registered and rejects when refused. |
| `call(call, cancelled, progress)` (the row's function) | `{callId, method, params}`, a by-reference cancellation `Promise<void>`, and a callback receiving one JSON value. | `{kind:"value", value?}` or `{code, message}`, whose `code` is the `appError`. Omitted value means JSON `null`. |

`callId` is the binding's opaque correlation id. Preserve the client's params
without adding a Session id or inventing origin fields. Notifications, handoff,
paired-device management, a generic `callChannel` service and new discovery
endpoints are outside this subset. Unsupported declarations or replies fail
explicitly, as in C4; they are never accepted and then ignored.

The table registers the namespace on behalf of the calling row, inside that
row's own apply: it decodes the declarations once at the bridge boundary and
registers the shared method owner's `MethodProvider`, with its existing
declaration validation and native admission. The namespace is registered before
the row becomes Active; the host waits for no service projection. The shared
method owner holds the only foreign DTO/parser/handler implementation, and Rpc
has none. Session and management registrations keep their distinct tables and
scope rules.

Use the current protected-local protocol-v3 entry admission, routing through the
management table, `hello.features` (the methods of the namespaces registered
now), row-status errors and progress/reply delivery. No call may switch to a new
provider generation after admission. Declared application errors preserve their
code and `appError`; transport or malformed-reply errors use a redacted binding
failure rather than exposing runtime stacks.

## 4. Work ownership and cleanup

The call admission's unload effect belongs to the calling row's context, as in
C4. Provider or admitting Rpc unload resolves the cancellation Promise for both
interruption modes; the provider's Promise resolves when the row's generation is
cancelled, before the runtime unloads the row's plugin. `report` replies
`operation_interrupted` while its original work settles; `drain` returns the
original work's result when it settles while the plugin still runs. A call still
running when the runtime unloads the plugin cannot reply: it answers
`operation_interrupted` and may already have taken effect ([rutis usage
R6](rutis-usage.md#r6-contributions-register-with-their-owner)), so an owner
whose work must not be lost settles on cancellation or reconciles in its next
generation, as Workflow's open attempt does. End each Promise lease on ordinary
completion as well. A client disconnect does not cancel admitted work. No forced
success, timeout, rollback or replay is inferred.

Business background tasks and their effects belong to the process package.
Closing a Session does not dispose that package. Process shutdown follows the
existing host shutdown and loader disposal sequence; the binding adds no second
lifecycle manager or generic process quiescence service. This does not claim to
settle P6/P7 timer, report or native-writer lifetime acceptance.

## 5. Impact and verification

| Owner | Necessary implementation scope |
| --- | --- |
| App Server assembly / TypeScript binding | Place rows without a group at the process, validate combined declarations, use original process factories, assemble their rows, serve `managementMethods` to them by name and collect Settings sources. Use the same required-host row selection for startup/report errors and restart completion (§2.1–2.2). |
| Shared method owner and management table | The table's foreign `register` reuses the existing foreign decoding/call boundary and registers on behalf of the calling row, with the existing native admission and cleanup. |
| SDK types and author guide | Describe the supported process contribution using the existing method payload definitions; retain existing Session exports for callers. |
| Examples and transport fixtures | Update all configuration literals and add a real Node process-owner fixture through normal protected-local Rpc. |

Implementation acceptance uses public flows, not private helper assertions:
the optional-row cases below assume module declaration loading completes. They
do not claim isolation from the hung module-load residual in §2.1.

1. One configured process owner serves two Sessions, survives either closing,
   and exposes a declared management method through local v3 `hello` and Rpc.
   Existing C4/C5 Session and branch parts still use the same runtime correctly.
2. Configuring no process row preserves current C4/C5 behavior. A disabled row is
   unknown; malformed/colliding structure is rejected. With a real missing
   declared dependency, native startup and Session calls succeed while the row
   stays Pending; its namespace, never registered, answers `unknown_method`. A
   throwing `apply` or refused registration fails only that owner; a namespace
   it registered before answers `service_unavailable`. Hold an
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
`sessions.input` retains admitted work, and the destination branch accepts each
`submissionId` once, so a replacement row resubmits instead of waiting for it. These bindings do not establish
P6/P7 business acceptance or full Ready.

<a id="51-proposed-p6p7-access-through-declared-feature-dependencies"></a>

### 5.1 P6/P7 access through declared feature dependencies

Status: the feature binding is accepted by the maintainer in
[the #237 architecture review](https://github.com/arcships/dim/pull/237#pullrequestreview-5584479437),
with the dependency and recovery constraints below. This replaces #35's per-call
owner replacement requirement. Full Scheduler/Workflow Ready remains separate.

Use the current rutis 0.16.0 row dependencies. The Scheduler Session tool row
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

<a id="52-public-pluginfiles-binding-proposed"></a>

### 5.2 Public pluginFiles binding

`pluginFiles` is a shared named service with an implemented text/transaction API.
`serve` registers its existing native provider with the authorized product root:

| Host configuration | Registration |
| --- | --- |
| Authorized `product_home` | Register the existing `PluginFilesPlugin(product_root)` once in the native process rows; availability does not depend on which process, Session, branch or managed rows are configured |
| No authorized product root | Do not invent a root or fall back to the working directory; no provider is supplied |

This changes only native composition. Keep `pluginFiles.open`, the scope/lease
API, S10 outer v1, errors, FileStorage authority and provider lifetime unchanged.
No row field, SDK method, dependency registry, optional lookup or rutis/core
mechanism is added. The managed plugin keeps its existing binding; public rows
use their own ordinary runtime and declarations, without a managed-plugin slot.
The provider depends only on FileStorage, preserving §5.1's stable boundary.
Session and branch parts resolve this same shared service through the existing
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

The actual Node acceptance uses isolated product homes. Public process and
Session-only configurations open a scope and replace/read versioned text without
a managed plugin. Closing a scope rejects new acquisitions and waits for a held
transaction to persist. An unprovisioned parent returns `io`, creates no directory
and does not enter the callback; invalid stored bytes return `invalid_data` and
remain unchanged. Without a product root the process row stays Pending while
native Sessions remain usable. Managed and public rows share one provider.
The Session-only flow also holds a transaction across `session.close`: closure
waits for its write to settle, and the next Session reads the persisted value.
The starting method has returned before close; only the file scope owns and
joins that transaction, rather than the admitted method's drain.

Workflow's persisted-attempt, failed-replacement and host-restart acceptance must
also run under this public binding. Readiness and report-specific evidence follow
each owner's design document. The storage binding alone proves no complete feature
behavior; no-run-effect behavior requires each package's business-flow acceptance.

## 6. Rule check

| Rule | Application |
| --- | --- |
| §4.1 Commands/results | No Timeline mutation is added. Process business methods retain their owner's typed writes; branch effects use existing Sessions/input contracts. |
| §4.2 Handover | Existing method admission and branch-input handovers remain unchanged; a lost reply does not undo work. |
| §4.3 Before/entry checks | No before callback. Host checks structural declarations; method owners validate business params. |
| §4.4 Core has no business meaning | Assembly and Rpc read service identities and namespaces, without Scheduler/Workflow cases. |
| §4.5 Batches | No event or batch change. |
| §4.6 Model context | Management bindings introduce no context path; inputs and tool results retain their existing owners. |
| §4.7 Public access | Supported process behavior uses declared dependencies and the original row's projections. Visibility of Session channels on the shared bus is incidental runtime behavior, not a process observation contract or sandbox (§2). No private plugin state is accessed. |
| §4.8 Infrastructure | #4 completes host bindings and makes the host readiness selection obey D3 for configured optional rows, including startup report failures and restart completion (§2.1–2.2). §5.2 supplies the existing storage provider in public composition; required-row waiting, rutis and the runtime gate are unchanged. |
| §4.9 Durable facts | Assembly metadata and process call bookkeeping do not enter Timeline. |
| §1 Scope/App Server | Process parts are loader rows, not Capabilities; App Server owns assembly/wire binding, packages own all feature work. |
| ADR 002 D1 | Process rows outlive Sessions; Session and branch parts retain their own lifetimes. |
| ADR 002 D2 | Bind in the original row; no adapter or management plugin. |
| ADR 002 D3 | Rpc reads the management table per call and depends on no provider. Host startup and restart readiness exclude optional extension rows while those rows retain their own hard dependencies and normal reload behavior (§2.1–2.2). A hung top-level module load remains a shared-gate startup limit (§2.1). |
| ADR 002 D4 | Structural declarations stay in rows; Settings documents remain business data and do not reload rows. |
| ADR 002 D5 | The process service `managementMethods` and its `register` implement the existing neutral payloads; Rust `ManagementMethods` is its native binding. |

## 7. Requirements raised to mechanism owners

| Item | Content |
| --- | --- |
| Contract served | ADR 002 D5's process `managementMethods` contract needs ordinary process placement and a foreign `register` on the native management table, owned by the calling row. |
| Existing mechanisms considered | Reuse the public C4 runtime, `RuntimeResolver`, `ServiceCatalog`, the management table and `MethodProvider`. The same assembly-owned required-row selection covers startup failures/readiness and restart. The existing `RuntimeRows` gate orders initial reconcile and extension apply without another task. Session factories have the wrong lifetime for process owners; events cannot install an absent row or contribute its management namespace. |
| Generic binding | Explicit process rows, management registration by the calling row, and required-host selection for startup and restart within App Server assembly. No new core service, facade, rutis requirement, loader API or public restart endpoint. |
| Ordering and lifecycle impact | No Timeline sequence, batch or event change. Keep one initial reconcile, the existing runtime declaration gate and required-row deadline. Optional row failures stay local at startup and restart; native management admission and provider report/drain settlement remain unchanged. |
| Implementation owner / issue | zerob13, App Server assembly and method binding under #4. Architecture review by the existing assembly/management maintainers; this does not assign new work to S2 or rutis. |
| Public storage placement | §5.2 registers the existing PluginFilesPlugin once per authorized product root for public and managed consumers. It uses no Session factory, private plugin slot or new filesystem API. No Timeline ordering, lifecycle protocol or mechanism-owner extension is requested. |
