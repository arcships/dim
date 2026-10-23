# Session TypeScript parts

Status: tool/method and leaf broadcast bindings implemented for [C4 #63](https://github.com/arcships/dim/issues/63)
and [#168](https://github.com/arcships/dim/issues/168), following the approved [#174 contract](https://github.com/arcships/dim/pull/174).
The runtime uses rutis 0.12 (0.11 leaf events from [#173](https://github.com/arcships/dim/pull/173)).
The verification section distinguishes executable acceptance from platform limits.
The [spike](session-typescript-spike.md) establishes transport feasibility;
its private service names and combined fixture are not this API.

The [process-row and management binding](process-typescript.md) uses the same
embedding input and runtime. Process owners use `process_rows`; the `rows`
member retains the C4/C5 placement described here.

## Scope and ownership

A TypeScript Session part supplies named tools and Session methods and observes its Session's
broadcast. One Node process serves all such parts; loader instances isolate their services.
The Session owns the part's state and lifetime. Native execution remains available when a part
is absent, Pending, failed or reloading. Instance labels are namespacing, not a sandbox.

The existing owners implement their language bindings:

| Owner | Responsibility |
| --- | --- |
| App Server assembly | Validate structural input, register names derived from declarations, append ordinary rows inside `SESSION_GROUP`, select the existing runtime resolver. |
| ToolRegistry | Read a named foreign declaration when it attaches a provider generation and retain that generation in its versioned contributed set (A.5, #241). |
| C1 `SessionMethods` | Bind a foreign handler to the existing generation-owned admission, progress, interruption and drain. |
| Session broadcast | Encode its own records for the native rutis event path. |
| TypeScript part | Own its business state, work, effects, errors and cancellation settlement. |

There is no registration plugin, event-forwarding service, second bus,
new Session facade method or readiness phase. Core never requires a TypeScript row. Timeline, Dispatcher and persisted event formats
remain unchanged. This work does not expose arbitrary runtime services through the Session facade.

## Host input and declaration discovery

The initial usable entry is the public App Server embedding API:
`serve(builder, ServerConfig { session_typescript: Some(...), .. })`. It runs an actual App Server and its
normal Rpc path. The [embedding example](../../crates/dim-app-server/examples/session_typescript.rs)
uses a protected Unix connection and a disposable storage home. General CLI installation,
marketplaces and editable plugin intent remain PluginManager work.

The input contains the existing `LocalRuntime::node` arguments (`node`, `jsRuntime`, `anchor`,
absolute paths) and `rows`, an array of ordinary loader row objects. One Session TypeScript
runtime is shared across the rows and Sessions. Its runtime name differs from an account runtime;
no private account manifest or account service is required by this entry. The runtime declares
the leaf events `settings.changed` and `settings.project_authorized`, which `SessionNodeRows`
forwards from Settings ([settings-service §4.4](appserver/settings-service.md#44-events)); a part
subscribes with `ctx.on`.

Each row retains loader's `id`, `name` (module entry), `config`, `disabled`, `inject` and `isolate`
semantics. The C4 row metadata is `provides`, with **exactly the existing
`rutis_bridge::runtime::RowSchema.provides` shape**, rather than a parallel tool/namespace list,
and, only for a package that owns Settings sources, `settings` (below):

```json
{
  "id": "web",
  "name": "/absolute/web/index.ts",
  "config": {},
  "provides": {
    "toolContribution:web_fetch": {"declaration": "sync", "execute": "async"},
    "sessionMethods:session.web": {"describe": "async", "call": "async"}
  }
}
```

The empty configuration above means this particular part has no structural options. Business
settings belong to the owning plugin's Settings source, not to this object.

A package that owns Settings sources declares them in a second structural member, `settings`, an
array of the entries defined in [settings-service §3.1](appserver/settings-service.md#31-source-declaration).
The host passes them to Settings with the row id as owner and does not hand `settings` to the
loader. A source is process state, so it belongs to one row: a second row declaring the same
source fails assembly, as two packages would. Rows without `settings` are unchanged.

`toolContribution:<tool>`, `sessionMethods:<namespace>` and `sessionEvents:<part>` encode the existing neutral service
identity and key as one bridge service name. The colon separates the family from the complete
key; the namespace keeps its dots. The catalog registers each full name as a shared service in
`SESSION_GROUP`. Tools and known namespaces are derived from these names and passed to the
existing `AgentBuilder::session_tool` / `session_methods` during assembly. Authors do not make
these Rust calls and do not separately maintain arrays of tool names and namespaces.

`provides` and `settings` are structural input, not a new package manifest or persistence format.
No filename, package schema, version field, content discovery or integrity scheme is introduced.
A package may import one JSON declaration into its TypeScript `provides` and give that same
declaration to the host; the two sides must not be separately hand-maintained. Its `settings`
entries are likewise one JSON file the package ships and the host is given; the TypeScript code
does not repeat them. A future PluginManager can produce these rows from its package boundary. Its proposed schema 2 is not assumed to be available, and the
private managed-provider schema 1 is not accepted here.

The static declaration is necessary because `RuntimeResolver` can describe a module only after
Node starts, while the host installs AgentBuilder's factories and Rpc's namespace list before
the loader starts. C4 does not move that list into a mutable registration service or bootstrap
a second runtime. On the existing online resolution, compare the Node description's `provides`
with the row snapshot before loading that row. A mismatch is a row resolution error; it does
not alter the configured names. The pinned SDK's initial offline placeholder has no `provides`;
only its existing runtime-source/unavailable-schema marker permits that provisional resolution.
The SDK later uses `resolve_again` to obtain the online declaration before publishing its
`RuntimeRows` catalog. An online description must match the snapshot, including when its actual
`provides` is empty; missing online declarations are not accepted as placeholders. The same
resolved factory retains the original schema, dependency declarations, scoped factory, metadata
and configuration validation.

After process reconciliation releases its loader operation, App Server's existing startup
readiness waits for the configured runtimes' `RuntimeRows` catalogs as well as their process
rows. It uses the existing startup deadline and failure cleanup. This orders Session creation
after declaration discovery; it does not wait for genuinely Pending optional Session rows.

Rows naming the same module must use the same snapshot; distinct row ids alone do not make
conflicting structural declarations valid.

At this external configuration boundary reject empty event-part suffixes, malformed identities, unsupported method modes,
duplicate tools, duplicate namespaces, host-tool collisions and the C1 reserved-name/namespace
collisions. Disabled rows are not instantiated and contribute no callable methods. Their declared
tool identities remain known to ToolRegistry, and only an active provider offers them, so a
disabled row contributes nothing and a stored `SessionConfig.tools` naming it stays valid. New model requests never declare that missing
tool. Enabled rows alone determine C1's known namespace list: a disabled namespace is
`unknown_method`, not an indefinitely reloading service. A declaration changing structurally
requires rebuilding the host assembly; business settings do not change it. Missing dependencies
of an enabled row keep their existing Pending/reload behavior.

## Tools

The service and data contract is [Session boundary A.5](contracts/session-boundary.md#a5-tool-contribution).
`declaration()` is synchronous; `execute(call, cancelled)` is asynchronous. The second argument
is the existing bridge's by-reference cancellation Promise, an implementation of A.5's cancel
action, not a new durable token, method or cancellation registry. It resolves on turn cancellation,
when the owning row unloads, and when the native call ends. The provider must release external
resources before settling.

The tool owner performs the only conversion between the neutral call and native execution:

| Neutral field | Source |
| --- | --- |
| `sessionId` | This Session's identity. |
| `branchId`, `turnId` | Existing `ToolExecutionContext`. |
| `callId`, `arguments` | Native `ToolCall.id`, `ToolCall.input`. |
| `config` | This Session's immutable `SessionConfig` serialized with its existing serde representation. |

Do not pass the native `{id, name, input}` object and silently treat it as the neutral shape.
The selected service identity already fixes the tool name. Declaration and result decoding use
the existing `ToolDefinition` and `ToolResult` serde types. The declaration's `name` must equal
the selected tool key, and a contributed tool supplies its required Permission risk. No second
schema parser or normalization pass is added after this boundary.

`ToolResult.content` is model-readable and uses the existing native JSON contract (text or typed
content blocks); `structuredContent`, blob references and background fields retain their existing
meaning. The shorthand block array in A.5 is not a restriction that removes native result fields.
A normal provider failure returns a valid `ToolResult` with `isError: true`. A bridge failure,
malformed result or unloaded provider becomes the existing host/tool failure and diagnostic;
it is never turned into an empty successful result. Cancellation uses ToolExecutor's existing
interruption result rather than inventing a provider error fact.

ToolRegistry reads the ordinary native contribution and neutral service identity when it
attaches a provider generation (A.5). Assembly permits only one configured owner per tool key;
no other provider is tried after that owner's declaration or execution fails. The foreign
service is looked up directly by its Session instance and bridge name. Its synchronous
`declaration()` is decoded there, and the handler retains that exact object together with the
original row context's existing `CancellationToken`, published under the same tool service name
and Session instance as a typed native key. Cancellation rejects later executions through this
retained handler and ends its in-flight native wait; it does not select a replacement generation
or force an uncooperative JavaScript task to stop. The TypeScript owner still settles its work
through its effects. There is no eager declaration during row apply, polling, asynchronous
attachment or changed `ToolContribution`
storage type. Synchronous declaration must not perform external asynchronous business work;
like a synchronous Rust provider it can delay its caller if it does not return.

Each configured foreign tool is a registry-owned versioned set (A.5, #241). Absent/Pending
providers omit only their tool and produce the existing attach diagnostic; once a row's
generation is active, the next request offers its tool without a Session reload. A malformed
foreign declaration is rejected when that generation is attached, omits only that provider's
tool, and records an explicit invalid-declaration diagnostic; the registry never caches an
invalid definition or an empty successful substitute, and does not retry the same generation.
A retired generation's set is withdrawn and the next active generation gets a new version.
ToolBinding adopts changes through Prompt/BranchOpening as for other sources, and each request
keeps its selected handlers. A new generation is not substituted into an old request.

## Session methods

`sessionMethods:<namespace>` uses C1's neutral `describe` and `call` service. C4 implements the
currently supported C1 subset; it does not implement connection-bound notifications, handoff,
RemoteGateway origin or a new process `callChannel` service.

`describe()` returns `MethodDecl[]`: `name`, `mutating`, `interruption` (`report` or `drain`) and
`progress`. Existing C1 validation applies. A declaration requesting unsupported notification
or handoff behavior fails the row rather than accepting work it cannot honor. Declarations
are decoded once for that provider generation, before its native `SessionMethods` is published.
Cancellation of the original row context also ends a pending `describe()` wait, so unload does
not depend on that declaration Promise settling.

`call` receives the existing neutral `{callId, method, params}` data plus the bridge cancellation
Promise and per-call progress callback. `params` includes the original `sessionId` and optional
`branchId`. `callId` is an opaque runtime correlation identity for this accepted call, not a
durable business identity or the client's JSON-RPC id. No unimplemented `origin` fields are
invented. A reply is `{kind: "value", value}`; an omitted `value` (including JavaScript `undefined`
omitted during JSON serialization) becomes JSON `null`. `{kind: "handedOff"}` is unsupported in
this subset.
Errors are the existing neutral `{code, appError, message}` data, decoded once into C1's wire
error. Provider-defined application codes must be preserved rather than replaced with a success
value or leaked runtime exception; C1's Rust error storage must own the decoded string. The
fulfilled call returns either the value reply or that error object. A thrown transport/runtime
exception is a binding failure, not a provider-defined application error, and exposes no stack
or runtime internals to the client.

The binding composes the existing `RuntimeResolver`'s resolved factory in the **same row apply
context**. It first applies the runtime row and immediately registers that row's cleanup with
`ctx.effect`; it then obtains its own foreign service and calls `SessionMethods::provide` with
one handler for the generation. Cleanup order is therefore C1 admission/drain first, Node row
unload afterwards. No extra row, child capability or per-call `MethodProvider` is introduced.
Reading a service provided by this same apply context uses rutis' existing self access rule;
external required reads remain declared dependencies.

Rpc continues to select the longest known namespace, authorize the Session and acquire/restore
it through Agents. Listed but inactive answers from the part's row copy in that Session:
`service_reloading` while it is not started, Pending, Loading or between generations,
`service_unavailable` while it is Failed, Unresolved or Stopped
([management methods](appserver/management-methods.md#dispatch-and-lifecycle)); an unlisted
namespace is `unknown_method`. `session.methods` and actual calls read the native C1 service through the
existing Session facade. No arbitrary `HostDispatch` is made a public Session service.

C1 owns call admission, progress closure, interruption and waiting for accepted work:

| Situation | Result |
| --- | --- |
| Provider or admitting Rpc generation unloads | Its cancellation Promise resolves. `report` replies `operation_interrupted` and settles the work; `drain` returns the provider's own reply. Both settle before provider unload finishes. |
| Client disconnects | Existing C1 behavior: work continues; that client's progress/reply are discarded. |
| Provider reloads | Retained native provider rejects new calls. No call crosses to the new generation. |
| Progress after reply / undeclared progress | Existing `Progress` drops it; no second queue. |

The by-reference Promise is deliberate: dropping a remote future can discard its eventual reply,
which cannot implement `drain`. The native call ends its Promise lease on normal completion too.
TypeScript code cooperatively settles after cancellation; no timeout or forced-success reply
is invented for an uncooperative plugin.

`rows.load` can reply before the native projection publishes its exported services. The original
row subscribes to the existing `ServiceChanged` event before loading and waits for all its own
declared tool and method-service projections before its apply returns and it becomes Active.
C1 binds the method namespaces during that apply; tool declarations remain unread until
ToolRegistry attaches the row's generation. The row context owns the subscription and cancellation ends the wait.
The foreign cleanup is registered immediately after loading, so a failed declaration or cancelled
binding still releases that row. This wait does not make another optional row required or change
Session publication.

## Native Session broadcast

This is the #168 implementation of the approved #174 contract.
Observation grants no branch write or context-input path; critical ordered
consumption retains the existing Dispatcher/C5 boundary.

### One native emission

Existing `SessionNodeRows` owns one
`on_pattern(EventPattern<SessionBroadcastEvent>::prefix("session/"))` subscription.
It uses its existing Runtime dependency. An enabled row declaring the event binding
below enables observation. NodeRows does not inject AgentIndex: an index reload
must leave the shared runtime's Session rows loaded (sessions-service D1, §8.3).
No new forwarding Capability or public subscription registry is introduced.

At callback entry, derive the Session id from the matched channel, look up the
current index with `ctx.get::<AgentIndex>()`, and call its non-restoring
`session(id)`. Without an active index, drop the observation without buffering or
retry. Admit only a present Session whose
`is_closing()` and `is_unloading()` are false. This is existing publication:
`Agent::get_session` exposes Ready after `activate()` under the same entries lock.
It neither creates/restores a Session nor adds a readiness wait.

Declare `LocalRuntime::emits(["session.event"])`. Emit each admitted native
`SessionBroadcastEvent::Event` once with one argument
`{sessionId, branchId, event}`; `event` is the unchanged `AgentEvent` serialization.
Native `emit` orders callbacks per Session channel and awaits each listener.
The callback therefore only admits the record to a Session-keyed FIFO owned by
SessionNodeRows and returns; it never awaits Node I/O or queue capacity. One
owned task per active channel drains that FIFO and awaits `Process::emit` in
order. Other listeners on the same native channel, including Rpc notifications
and delivery fences, never wait for leaf delivery. No extra ordering is imposed
between branches beyond native channel order, or between Sessions.

The FIFO and task are transient transport resources, not a service registry or
business state. One NodeRows-owned inbox task keeps the mutable channel map; native
callbacks need no map lock. Each worker also observes public `Session::closed()` so
idle unload, which intentionally emits no native `Closed`, retires its resources.
A restored Session reuses its channel name with fresh resources. Worker task handles
identify only those I/O resources, so an old completion cannot retire its successor.
Register their cleanup immediately with the NodeRows effect.
Session closing ends admission and discards queued observations; original-row
cancellation stops that row's handlers. Native `Closed` retires that channel's
transport resources, and NodeRows unload releases all of them. An in-flight
emit may finish after closing; the original row's handlers still reject new work
after their closing boundary. There is no replay, transfer of queued work across
a NodeRows reload or retry after failed delivery. Delivery failures use existing rutis event diagnostics,
without Timeline success facts. A slow leaf can accumulate pending observations
only in its NodeRows transport, never delay native delivery; the transport adds
no bounded-queue wait or silent event dropping while the Session remains open.
[Slow leaf backlog](#slow-leaf-backlog) settles its diagnostics and retirement.

Eligibility is sampled at callback entry: a captured Loading-time emission
processed after publication may be delivered; one processed earlier is dropped.
An admitted delivery may finish after close starts. No replay or emission-time
barrier is added. Listener membership is selected by the runtime for
`Process::emit`; a row bound before a delayed forward may receive that earlier
native fact. Completed emissions are not replayed to later listeners.

### Slow leaf backlog

Status: proposed for [#184](https://github.com/arcships/dim/issues/184); not implemented.
The rest of this section is unchanged by it.

A channel's FIFO grows while its worker awaits a `Process::emit` that does not settle.
`emit` resolves only after every `session.event` listener in the Node process has finished,
so a listener that awaits work instead of registering a task and returning (contrary to
[Original-row setup](#original-row-setup)) holds the channel. Because every Session's
emission reaches every row's listener before the `sessionId` filter, one such listener can
hold every channel, not only its own Session's. Rust cannot tell which listener is slow.

| Option | Verdict |
| --- | --- |
| Bound the FIFO and drop with a gap marker; observers re-read history | Rejected. Adds a payload kind, and a TypeScript row has no Session history read, so recovery needs a new API. |
| Bound the FIFO and fail the observing row | Rejected. The channel does not identify the slow row, and failing an Active row from outside needs a loader API (§4 rule 8). |
| Bound the FIFO and retire the channel (disconnect) | Rejected. Observers silently miss the rest of the generation; one bad listener would disconnect every Session. |
| Per-emit timeout | Rejected. An invented deadline; dropping the await does not stop the Node listener, it only hides the stall. |
| Keep the FIFO unbounded per generation, retire it without waiting for delivery, warn on growth | **Chosen.** |

The policy:

1. **No bound, drop or retry.** While a Session generation is open, every admitted
   observation stays queued in order, so an observer never sees a gap and needs no resync.
   The backlog ends with its generation: Session close, idle unload, native `Closed`, NodeRows
   unload or reload, and Node process exit retire it, as today. Client attachments have the same
   shape: an unbounded per-attachment queue that detach or connection close ends. A trusted
   in-process package is not sandboxed ([ADR 003](../adr/003-approval-scope.md)), so a queue bound would not be an isolation
   boundary.
2. **Retirement does not wait for delivery.** The worker races its in-flight `Process::emit`
   with `Session::closed()`, as it already does between deliveries. Once cleanup completes,
   it drops the pending emit, its FIFO and its `Session` handle and returns, so the inbox removes
   the `channels` entry. The Node listener may still finish, which closing already allows. Aborting
   on native `Closed` already drops an in-flight emit the same way. This settles the `channels`
   retention question: an unloaded Session with a stalled listener and no further record is no
   longer referenced once its cleanup completes. Today the entry stays until the emit settles or a
   restore replaces it, so with idle unload each unloaded Session behind a stuck listener would stay.
3. **Diagnostic.** Each channel keeps its count of admitted, undelivered observations. When the
   count reaches 1024 and at each doubling after that (2048, 4096, …), the inbox emits one
   `tracing::warn!` with `event = "session.event"`, `session_id` and `pending`. The level only
   decides when a warning first appears, never delivery; it is a constant, not configuration.
   Doubling gives a logarithmic number of lines for unbounded growth without a timer or rate limit.
   Listeners that follow the setup contract keep the count near zero, so the warning points at a
   listener that awaits work.

Recovery needs no new path. Fixing or removing the package and reloading its row, or unloading
or restarting the Session, gives a fresh generation and channel; queued observations are not
replayed, as for any reload. A part whose state must outlast a generation already rebuilds it
from its owner's state.

Acceptance extends the existing stalled-leaf flow: with a listener held, records past 1024
produce one warning naming the Session and its pending count, and native clients and fences
continue. Idle-unload the Session while the listener is still held, then release it: no queued
observation is delivered after cleanup completes, and a restore delivers fresh records once and
in order.

Rule check for this change:

| Rule | Application |
| --- | --- |
| §4 rule 1 | No other owner's state is written; NodeRows changes only its own transport resources. |
| §4 rule 2 | Accepted observations stay with their generation; nothing is transferred or retried. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No core change; the policy lives in SessionNodeRows. |
| §4 rule 5 | No Timeline writes. |
| §4 rule 6 | No context input; observations remain non-critical. |
| §4 rule 7 | Uses only public `Session::closed()`, `is_closing()`/`is_unloading()` and the native broadcast. |
| §4 rule 8 | Loader, rutis and lifecycle APIs unchanged; per-listener attribution is not requested. |
| §4 rule 9 | The warning is a log, not a Timeline fact or notice. |
| §1 | NodeRows owns the shared transport; each part still owns its state and work. |
| ADR 002 D1 | Per-Session transport stays in the shared NodeRows owner. |
| ADR 002 D2 | No new capability, registry or forwarding owner. |
| ADR 002 D3 | Core never depends on an observer row; native delivery is unaffected. |
| ADR 002 D4 | No new configuration. |
| ADR 002 D5 | The `session.event` payload and bind contract are unchanged. |

Requirements raised to mechanism owners: none. Attributing a stall to one listener would need
rutis `emit` to report listeners separately. No policy here needs that, so it is not raised.

Ablation: a bound, drop path, gap marker, row-failure path, per-emit timeout, configurable level,
per-record timestamps and a "drained" log were each considered and left out; none is needed to
keep native delivery independent or to make growth visible. Two pieces remain. The race with
`Session::closed()` releases an unloaded Session's resources. The pending count and doubling
warning make a stalled listener visible without changing delivery.

### Original-row setup

A consumer adds one entry to its existing `provides` dictionary:

| Element | Contract |
| --- | --- |
| Identity | `sessionEvents:<part>`, Session scope, provided by the original TypeScript row. The suffix is nonempty and follows existing exportable-service name constraints; identities remain unique. |
| Method | `bind`, sync: `bind({sessionId: string, config: SessionConfig}, closing: Promise<void>) -> null`. |
| Invocation | Once during native row apply, after its projection is visible and before native row Active. |
| Closing | Resolves on existing `SessionLifecycle::closing()` or original-row cancellation; never follows a replacement generation. |
| Failure | Structural identity/mode errors are rejected at the existing configuration boundary. Thrown bind calls, non-null replies or a returned future fail the optional row through existing diagnostics and cleanup. |

Only such a row adds native `InSession<SessionIdentity>`, `InSession<SessionConfig>` and
`InSession<SessionLifecycle>` dependencies. Reuse existing projection and
by-reference Promise support. The original `config` stays unchanged; no second
manifest/list, client method, tool or public Session facade service is added.
Existing tool/method-only rows need no new entry.
`config` in the bind request is the same creation-time SessionConfig serde
form used by ToolCall, separate from the plugin's structural options. This
binding retains that form's rule: absent `interactive` means true;
only false is serialized. SessionTitle tests `config.interactive !== false &&
config.collaborationRole === 'leader'`, never truthiness of the omitted field.
No consumer fills or rewrites the snapshot. It does not report later Prompt/model
adoption. Approval of the binding includes this serialized-config decision.

The row's binding registers `ctx.on('session.event', listener)` and compares
`payload.sessionId` with the supplied id before business code. This is required
binding-level routing, not a sandbox: trusted plugins can subscribe directly or
emit events. Do not infer identity from the first event or a runtime handle.

Bind only registers subscriptions and effects. Event handlers register owned
tasks and return. When closing settles, stop admitting work, cancel/release
resources and join accepted work through existing row effects. `ctx.on` removes
the listener on unload; captured handlers do not start work after their original
generation ends. No callback or work is rebound.

### Necessity and acceptance

`ctx.on` is process-wide and supplies no Session identity, configuration or closing.
JSON configuration cannot carry the closing Promise. One setup method transfers
these existing native values without changing user options or adding a ready
flag, generic event service, per-row emitter or deduplication state.

`Fence` and `Closed` stay native. Foreign lifetime ends through closing and row
effects, with no additional end frame or fabricated `AgentEvent`. C6 retains notice/Rpc ownership:
its `{source, kind, data}` `Notice` records ([Session broadcast notices](appserver/session-notices.md))
are not `Event` records and are not forwarded to leaf parts here. SessionTitle observes `input/applied`;
its stored title notice reaches clients through C6/Rpc, not this TS binding.

Process-level `settings.changed` and `settings.project_authorized` are outside
#168 and are not wrapped in `session.event`. Their S8b/#118 delivery is implemented
by #177 in the same SessionNodeRows owner. The runtime declares the union of those
names and `session.event`; neither forwarding path replaces the other
([settings-service §4.4](appserver/settings-service.md#44-events)).

New public content is only the setup contract and fixed leaf event above; core,
loader, facades, native broadcast types, Dispatcher, Rpc and persistence do not change.

Extend existing real Node/App Server flows: two rows in one Session each receive
one copy; another Session receives only its records; order/content match native
history. Hold a leaf listener pending while an attached client receives the
following native records and fence acknowledgement without waiting for Node;
then release it and verify ordered, single-copy leaf delivery. Close with a
pending emit and queued records: no queued observation starts new work after
closing, and the channel's transport resources are released. Cover publication
sampling, late listeners without replay, and interactive true/false plus leader/member
eligibility using the serialized creation snapshot. Hold
owned work through close and row-only reload: closing settles it before effects
finish, stale listeners stop, and native/other-Session use continues. Check bind
failures and throwing listeners; preserve Pending/disabled rows, tools that follow
their row's activation (#241) and C1 drain. The executable flows below exercise these cases through the existing Session, C1, Node and Rpc boundaries.

## Lifetime and failure

Loader owns generations and rows. Session closure stops branches before unloading Session
parts. Each TS part immediately registers its acquired resources with its own `effect`, stops
new work on disposal, releases I/O, settles accepted work and rejects retained service objects.
A retained remote object must reject calls after the part's effect begins unloading.
The native C1 effect drains while the Node part is still alive. Tool cancellation follows its
existing turn owner and the original row's context cancellation; no work transfers to another
generation.

The implemented tool/method subset permits synchronous service/effect registration in TS apply,
and business work driven by accepted calls after Session publication. It exposes no neutral
Session activation service for arbitrary autonomous TS background tasks. The event
binding admits work only through published Session observations and supplies closing, not an
autonomous-start interface. Row Active alone never permits business work from TS apply.

Session assembly uses the registered required-part membership implemented by [#137](https://github.com/arcships/dim/pull/137).
Session assembly settles rows already Loading, including their tool/method projection waits.
Host-appended optional rows stay in the original disposal group and diagnostics and do not enter
the Session's required Active wait: a row genuinely Pending on a missing dependency does not
delay native Session use. Their hard dependencies, key validation and failure reporting do
not weaken. Disabled parts have no row instance. An optional failure changes only its features;
there is no automatic retry of an operation that may have produced external side effects.

Restore rebuilds rows and reads and follows providers through the same code as create; history does not
install or enable code. Structural reload can fail in-flight calls as ADR 002 permits. Runtime
process loss is an infrastructure failure; no special runtime-crash recovery mechanism is added.
Completely removing a configured tool identity differs from disabling its row: the existing
Session tool validation rejects an unknown saved name. C4 does not silently edit persisted
Session configuration or introduce a history-driven reinstallation path. Retain the disabled
structural declaration when existing Sessions need to remain loadable; package-uninstallation
policy belongs to PluginManager rather than an undocumented fallback in ToolRegistry.

## Impact and verification

| Area | Implementation and remaining boundary |
| --- | --- |
| App Server `ServerConfig` and assembly | Public embedding input, row boundary validation, names/catalog derived from `provides`, optional Session rows and native resolver factory composition. |
| ToolRegistry and its page | Native/foreign owner selection and declaration decoding when a generation is attached. Existing ToolBinding, Prompt and ToolExecutor flow is reused. |
| C1 `methods` module | Neutral DTO/error decoding and generation-owned foreign handler construction. No Rpc dispatch replacement. |
| Session broadcast | One native pattern subscription and ordered channel workers in existing SessionNodeRows, one fixed leaf event and original-row identity/closing setup. C6 retains notices. |
| Dependencies | Existing pinned `rutis-bridge` at the language-boundary owners; no new library or rutis change. Regenerate graphs through `xtask graph`, then run `xtask check`. |
| Documentation | Owner pages, App Server README and Session-boundary Appendix F updated to the actually implemented subset. |
| Tests and fixture | One real App Server/Node flow using a disposable home and local Rpc; focused malformed-boundary cases in that flow. |

The executable Node/Rpc flows cover two Sessions sharing a Node PID; an Active tool declared on the immediate
first model request after create and restore and executed through ToolExecutor; a pending optional
part that does not block native use; a later Active tool offered from the next request without a reload; disabled rows;
actual C1 Rpc call/listing, progress, `report` and `drain`; stale provider rejection; continued
native/other-Session use after one part closes. Native Session broadcast flows additionally cover single-copy delivery to two rows,
Session/branch routing and native content/order, loading-time sampling, client
notifications/fences while a leaf stalls, and continued delivery to another Session.
Row-only reload ends owned work without replay, idle unload/restore creates fresh
channel resources, and close/shutdown settle pending emits and queued observations.
An Agents index restart leaves the observer and tool/method rows' generations unchanged;
subsequent records retain single-copy delivery and native order.
Creation snapshots cover interactive/non-interactive leaders and members; malformed
bind modes, thrown calls, non-null/future replies and throwing listeners keep native
and other-row execution available. These flow checks use real services and fences.
Gate each observed transition on an actual service/event/fence, not a sleep. Boundary cases cover
declaration mismatch, invalid tool/method reply, name collisions and cancellation without leaking
a Promise lease. Reuse C3/C1 tests for their unchanged native semantics.
Native-store fork creates fresh Session-scoped providers from the copied configuration, and
delete drains the fork's admitted method before unloading it while its source remains usable.

The existing CI Node step executes the product Node/Rpc flows alongside the managed-provider
flow. Ordinary Session assembly covers the immediate first request after create and restore,
including a tool-only module with no method namespace or test-only required dependency.
Separate Pending and disabled rows verify native continuity. Row-only reload exercises in-flight
tool cancellation and rejection through retained handlers, and unload during a stalled `describe`
exercises binding cancellation. Invalid tool/method declarations, malformed replies, omitted
method values, online snapshot mismatch and conflicting module snapshots remain boundary cases
in those flows. The protected embedding example prints durable history; submit/idle success
alone does not certify that a TypeScript tool executed.

The spike and local native-event experiments remain evidence only for their stated boundaries.
The executable Node/App Server acceptance is calibrated on macOS/Unix. Native Windows
Node delivery is unverified; the CLI Windows checks do not establish C4 support there.

## Ablation

`provides` is retained solely because the existing assembly needs names before Node is online;
all names and service modes are derived from that one existing dictionary shape. A second
manifest, separate name arrays, registration manager and per-call provider are unnecessary.
Same-row factory composition is retained because C1 must drain before unloading its foreign
handler. The tool declaration stays at first registry read, so no deferred contribution wrapper
or eager tool registration is needed. Promise cancellation preserves the existing `drain` reply.
Startup uses the existing process readiness loop and SDK catalog publication; the row's own
service-publication wait covers tools and methods without moving declaration reads. Retaining
the original row's token ends stale native tool waits without a tracker, per-call subscription or
new liveness API. Native pattern subscription avoids per-row transport and duplicate
emissions; the one setup method carries identity/closing without a new core lifecycle API.
The per-channel FIFO/task is necessary solely to preserve leaf order while returning
immediately from the shared native channel; it introduces no second event source or registry.

## Rule check

| Rule | Application |
| --- | --- |
| §4 rule 1 | No binding writes another owner's state. A business method uses its owner's existing service or command event; the binding does not produce its result facts. |
| §4 rule 2 | ToolExecutor and C1 retain accepted generation-owned work; no rebinding or revocation of a handed-over call. |
| §4 rule 3 | No before callback; boundary checks cover shapes/identity, not feature decisions. |
| §4 rule 4 | Core has no tool/plugin-specific branch. Neutral bindings live with their contract owners. |
| §4 rule 5 | No new Timeline batch. Business owners retain their existing atomic writes and dispatch order. |
| §4 rule 6 | Model-visible declarations use Prompt adoption; model-visible content uses adopted inputs and ToolResult only. |
| §4 rule 7 | Named services, C1, public AgentIndex/Session observation and native broadcast; no private state reads. The new setup method is limited to C4 identity/closing binding. |
| §4 rule 8 | Existing resolver, catalog, factory, scope, event and effect APIs; no infrastructure API, optional-dependency semantics or rutis requirement. |
| §4 rule 9 | Declarations and transient notifications stay out of Timeline; C6 notices retain their own owner. |
| §1 | Each business part is a Session foreign row; existing NodeRows owns shared runtime I/O. Bind only registers observation/cleanup. Native/public publication gates event admission; existing closing stops owned work. |
| ADR 002 D1 | Session state belongs to that part; Node and its loader resources are shared process resources. Branch facts remain branch-owned. |
| ADR 002 D2 | No new wiring capability or forwarding owner: existing NodeRows and original-row binding own their resources. No registration host or method aggregation plugin. |
| ADR 002 D3 | Core never hard-depends on optional parts; ToolRegistry reads tool providers by name and follows their activation (#241), and per-call C1 lookup is unchanged. Required dependencies remain required. |
| ADR 002 D4 | Only structural rows enter loader input; business Settings changes do not reload parts. |
| ADR 002 D5 | Canonical named service/key, data, errors and cancel action; Rust/native details are bindings. Existing unsupported C1 features are explicit. |

## Requirements raised to mechanism owners

None. #137 supplies the required assembly membership rule. Static declaration discovery,
owner-level bindings and native event composition are C4 choices within existing mechanisms.
The setup method is a C4 language-boundary contract, not a requirement for a core or
rutis API. It consumes existing identity and closing; native pattern subscriptions and public
publication lookup provide delivery without another readiness mechanism. C6 retains notices.
