# Branch TypeScript binding

Contract for [C5 / #64](https://github.com/arcships/dim/issues/64), the branch portion of
[#4](https://github.com/arcships/dim/issues/4). Status: implemented on top of full C4 (#180); see
§Implementation.

The contract includes the Session-side TreeCommands and branch-owned service reads required by
the existing Goal design. Its sole native facade addition is a named counterpart of the existing
typed public-service read (§Named public branch services).

## Scope and ownership

A TypeScript branch part consumes its own branch's committed events through the existing
Dispatcher and submits validated input through `Branch::input`. It reaches branch handles through
its Session's existing `TreeCommands`. It is an ordinary loader part inside the existing branch
instance, with the same owner for subscriptions, derived state, tasks and cleanup.

This is distinct from [C4 Session observation](session-typescript.md): a Session notification can
be queued independently of native notification delivery, while a Dispatcher callback participates
in the branch's ordered consumption. No Session notification queue is used for critical branch work.

| Concern | Existing owner or mechanism |
| --- | --- |
| Branch instance and typed keys | `BranchScope`, nested in its `SessionScope` |
| Ordered consumption and handler failures | Branch `Dispatcher` |
| Committed history for state reconstruction | Branch `Timeline` |
| External input validation and commitment | `Branch::input(InputMessageData)` |
| Branch handles, directory and creation | Session `TreeCommands` / `TimelineTree` |
| Activation and closing | Existing `BranchLifecycle` |
| Opening work that must finish before publication | Existing `BranchOpening` receipts |
| Foreign calls and references | Pinned rutis loader/bridge and the original row's effects |

The part must declare every dependency, including load-order dependencies. Missing providers retain
the existing Pending behavior. A consumer cannot obtain a sibling branch's Dispatcher or mutable
state; communication with another branch uses its validated input and does not wait for its work.

## Existing consumer coverage

[Goal's accepted design](plugins/Goal.md) is C5's scheduled consumer (Goal §1, §6,
Appendix A.3 and Appendix G). It requires all of the following, not just an event-to-input example:

| Consumer | Existing requirement | Proposed binding |
| --- | --- | --- |
| Goal's Session tool/method part | Resolves branches through `S:TreeCommands` and writes native Branch input. | Declare `treeCommands` as an ordinary injected Session service; use its returned BranchRef. |
| Goal's branch part | Provides the branch-scoped `goal` service with `get({}) -> {goal, through}`; during unload the read fails `service_reloading`. | Declare `goal` next to the row's setup service and project its public service only after bind/reconstruction completes. |
| `session.goal.get` | Reads the branch owner's fold through its consumed cutoff. | Obtain the branch, look up its named `goal` service, and call that owner's get. Do not recompute the fold in the Session part from committed history. |

Native `Branch::service<T>` reads a service marked `BranchService + PublicService` through
`BranchScope::key<T>()`. The named counterpart below retains those marker bounds and per-call
read semantics. No Session registry, cross-scope static dependency, caller-side reconstruction or
new input/reply protocol is involved. Goal's state and its get contract remain with Goal.

## Structural declaration and row placement

The existing `SessionTypeScriptConfig.rows` contains ordinary package rows. A branch row declares
exactly one `branchPart:<part>` setup service and may declare its public business services next to
it. `<part>` is non-empty and `bind` is the setup service's only method, declared `async`.
Its declaration determines placement, so there is no new `scope` setting:

```json
{
  "id": "goal-state",
  "name": "/absolute/goal/branch.ts",
  "provides": {
    "branchPart:goal": {"bind": "async"},
    "goal": {"get": "sync"}
  },
  "inject": ["treeCommands", "settings"],
  "config": {}
}
```

The module imports this same declaration's `provides` dictionary and uses its `inject` list in
`definePlugin`, as existing Settings consumers do. The loader row's inject controls scope;
it does not replace the module's dependency declaration. Both read the same list.
The host keeps current C4
tool/method/event rows in the Session group and appends branch parts to the existing nested
`BRANCH_GROUP` returned by `AgentBuilder::install`. It does not create another group or tree.
The existing runtime process, anchor, structural options, `inject`, `isolate` and disablement
continue to apply. Existing Settings declarations are discovered once from the configured rows,
before placement; they are not registered again for each branch instance.

Reject mixed branchPart and Session service families, duplicate public names in a branch,
empty identities, other bind modes and names colliding with registered native services at the
structural boundary. Business-service method dictionaries use the existing sync/async modes;
they are the explicit public exports. Preserve C4's same-module
declaration equality and online metadata checks. A package with both Session and branch parts
uses separate entry modules. Register each `branchPart:<part>` with the existing catalog under
`BRANCH_GROUP`; register the declared business-service names in that same group. Instances cannot
collide across branches or Sessions. No second declaration list or public-service registry is added.

## Session TreeCommands and named public branch services

### TreeCommands owner

Native TimelineTreePlugin already owns and provides `S:TreeCommands`. Add its language binding at
that same provider: a Session-scoped `treeCommands` HostDispatch with get/list/branch, using the
existing host key and catalog instance APIs. Both Session and branch rows declare `treeCommands`
in `inject` and use the normal `ctx.use("treeCommands")` lease. The provider has no AgentIndex
dependency and does not reload with the Agents index.

The owner captures its existing TreeCommands and cancellation, not a particular caller row.
Consumers own their calls; owned async creation uses the existing AbortSignal and is cancelled
from the consumer's cleanup. TimelineTree retains admission, settlement and terminal facts. A
shared service call does not transfer a task to a successor row or add a caller-readiness phase.

### Named public branch services

Propose this generic native read alongside `Branch::service<T>`:

```rust
pub fn named_service<T: ?Sized + BranchService + PublicService>(
    &self, name: &str,
) -> Result<Arc<T>>;
```

It uses the existing `TypeKey::keyed_dynamic::<T>(name)` qualified by this branch's instance,
the named-key construction already used by SessionScope. It reads the current provider per call
and returns the same Reloading error when absent as the existing typed read. There is no raw key,
context or instance selector in the public API; PublicService is still required. This is the only
proposed native facade addition. It neither writes state nor changes publication, dispatch,
dependency waiting or lifecycle semantics.

The capabilities language boundary defines one `ForeignBranchService` binding type implementing
BranchService and PublicService. Each explicitly declared business service gets its own value under
its named, branch-qualified key. It retains that service's existing HostDispatch, authoritative
method modes and original producer-row lifetime; it holds no collection of other services.
The original BoundRow publishes each such value only after its bind/reconstruction completes.
No empty fold or not-yet-initialized service is exposed as ready.
The setup service itself is not projected as a public business service.

TreeCommands' BranchRef resolves `service({name})` with the proposed generic read and returns
a bridge record whose method names/modes come from that original declaration. Calls go directly
to the original service. Its requests, replies and errors retain the owning service's contract;
there is no second Goal schema or extra reply envelope around its get result. Lookup itself uses
the C5 Reply envelope. A lookup is not a lease guaranteeing a later call succeeds: unload can end
the selected generation. Failures propagate through the existing owner/bridge call contract; callers
do not retry or replace them with a default state. In particular Goal's Session method retains its
`service_reloading` behavior rather than returning an empty fold.

The entire Goal read path is therefore Session `ctx.use("treeCommands")`, get the named branch,
look up its `goal` service, then call that owner's get. Session parts do not inject a branch service,
and one branch never receives another branch's Dispatcher. A call retains its selected producer;
a later public lookup can select the new provider after reload, without rebinding an accepted call.

## Original-row binding

Reuse C4's original `BoundFactory` / `BoundRow` path. For a branch part, the scoped factory obtains
the existing Session and branch instance keys from `Build`. Its declared dependencies are the
original foreign row's dependencies plus branch Timeline, Dispatcher, TimelineIdentity and
BranchLifecycle. TreeCommands is an ordinary declared named dependency. No native part depends
on the foreign row.

After applying the original row, register its foreign cleanup immediately and wait for its
`branchPart:<part>` projection through the existing service-change path. Invoke
`bind(binding)` once and await its null result before that row finishes loading. Failure follows
ordinary row failure/cleanup; it does not add a Session readiness phase or change Pending rules.
The original apply publishes all declared service objects. After bind, the native wrapper waits
for their existing projections and publishes the typed public-service values; callers cannot use
a public fold before reconstruction completes.

`binding` is a bridge record with the following named fields. Function and Promise references use
released rutis `Value::Record`, `Value::callback` and `Value::future`; they are not JSON handles.
The native functions capture this original row's `Ctx`, scoped services and cancellation token.
Structural `config` still reaches the original `apply` unchanged and is not replaced by binding data.

| Field | Value and owner |
| --- | --- |
| `identity` | `{sessionId, branchId}` from this branch's TimelineIdentity. |
| `dispatcher` | The named operations below, bound to this branch and original row context. |
| `timeline` | Read-only named operations on this branch's Timeline for reconstruction. |
| `activated` | `Promise<boolean>`: existing BranchLifecycle activation, false if closing or original-row cancellation wins before activation. |
| `closing` | `Promise<void>`: resolves at existing branch closing or original-row cancellation, whichever comes first. |

The original row provides `branchPart:<part>.bind`; it does not inject that service or the fields
of its own binding. Ordinary declared `ctx.use` dependencies retain the loader's existing behavior.
These owner-sensitive native functions are passed by reference rather than installed as shared
`ctx.use` services: shared `HostDispatch::invoke(method, args)` has no caller-row `Ctx` argument,
whereas native `Dispatcher::subscribe` requires the registering context. Capturing it in the
existing row factory keeps subscription removal on that row without a registry or another plugin.
This named-reference form is a C5 contract choice requiring confirmation, not an implemented
general-purpose native service lookup.

### Operations, data and errors

`Reply<T>` uses the existing named-service envelope: `{ok:true,value:T}` or
`{ok:false,error:{code,message}}`, as Settings and S2 do. A value may contain bridge references.
The notation below describes this proposed binding, not methods already exported by the runtime.
TreeCommands is the ordinary named service; Dispatcher/Timeline functions belong to bind, and
BranchRef is a by-reference facade value containing id, input and service functions.

| Named operation | Mode | Request / value in `Reply` | Native operation |
| --- | --- | --- | --- |
| `dispatcher.subscribe(keys, handler)` | sync | String key array and callback reference; null value. No separate unsubscribe method. | `Dispatcher::subscribe(originalCtx, partName, keys, handler)` |
| `BranchRef.input(data)` | sync | Native `InputMessageData`; committed `AgentEvent` value. | Native Branch facade |
| `BranchRef.service({name})` | sync | Exact public service name; a record of that owner's method references. | Proposed `Branch::named_service<ForeignBranchService>` |
| `treeCommands.get({id})` | sync | Branch id; `BranchRef` value. | `TreeCommands::get` |
| `treeCommands.list({})` | sync | Empty request object; native `TimelineInfo[]`. | `TreeCommands::list` |
| `treeCommands.branch({parent, at, options}, signal?)` | async | Native parent id, cutoff and `BranchOptions`, optional bridge AbortSignal; `BranchRef` value. | `TreeCommands::branch` with existing admission cancellation |
| `timeline.committed()` | sync | No arguments; native committed sequence. | `Timeline::committed` |
| `timeline.history({through})` | async | Required cutoff; native `AgentEvent[]`, including the ancestor prefix. | `Timeline::history(Some(through))` |
| `timeline.local({start, through})` | async | Required inclusive local bounds; native `AgentEvent[]`. | `Timeline::local(start..=through)` |

The event callback returns void or `Promise<void>`; bridge undefined/null represents unit. Await
the actual callback result before completing native handling. Callback exceptions, transport
failure and invalid non-unit replies follow the native Dispatcher failure path, not a successful
`Reply` or a warning. `bind` is async because state reconstruction may read Timeline history; it
returns null after registration/reconstruction, never after waiting for activation.

Parse request data once at this foreign boundary and call the existing owner directly. Native
`InputMessageData`, `BranchOptions`, `TimelineInfo` and `AgentEvent` keep their serde field names;
in particular this does not use S2's raw-block or publication-receipt shapes. Sequences retain
rutis' integer representation (`number` or `bigint` in TypeScript); do not coerce them through Number.

| Error code | Source |
| --- | --- |
| `invalid_params` | Boundary request decoding or a missing/non-function callback; native operation not invoked. |
| `invalid_input` | Native `AgentError::InvalidInput`, including Dispatcher key validation. |
| `unknown_branch` | Native `AgentError::UnknownBranch`. |
| `closed` | Native Closed, or a reference whose original row has ended. |
| `cancelled` | Native Cancelled. |
| `reloading` | Native Reloading. |
| `failed` | Other native failures, with the native diagnostic message. |

Codes classify the returned failure; they do not grant retry or prove that a write/creation did
not commit. Bridge loss remains an unknown outcome. Setup failure fails the original row through
its existing loading path. Row-bound Dispatcher/Timeline references end with that original row;
public service references retain their selected producer's generation. TreeCommands/facade values
follow their existing Session/branch owner lifetime. Cleanup releases owned references, and no
reference is silently rebound to a successor.

The consumer passes call cancellation to async creation using the existing bridge AbortSignal and
CancellationToken admission path, including from its original-row cleanup. TimelineTree retains its own cancellation
and terminal-fact decisions. There is no retry, branch deletion or fabricated completion on loss.
No arbitrary Timeline append, dynamic event declaration, before callback or extra facade mutation
is exported by this C5 subset.

## Native contracts to preserve

The operations above delegate native validation and behavior. Dispatcher keys accept an exact
key, `*` or `domain/*`. Its callback receives the original committed AgentEvent; errors/panics
fail the Dispatcher. The input reply proves commitment, not publication, dispatch, adoption or
model consumption. Native serde definitions stay authoritative, with no aliases or defaults.
Capability Timeline reads reach a fixed committed cutoff; public Branch history/local reads only
published events. A directory record does not prove publication or activation.

`TreeCommands::get` can find a context after TimelineTree stores it and before publication.
It is not an initialization guarantee: the Dispatcher is started while the context is being built,
before TimelineTree stores that context. An early lookup can therefore fail. The binding must not
hide that outcome with retries, a replacement branch or another readiness phase.

## Subscription, opening and recovery

1. Register the foreign callback on the original branch part's native `Ctx`, not a process or
   Session subscriber. A row-only reload must remove the old listener without changing another
   branch's listeners. A shared `HostDispatch` lease alone does not establish that ownership.
2. Preserve the native serial await and failure path. Do not return early into a C4 transport FIFO
   or turn a handler failure into a warning followed by later critical events.
3. An event handler registers work with its owning part and returns. Network and tool waits belong
   to that work. Background work started during initialization waits for existing activation and
   ends on existing closing; the row registers cleanup as soon as it acquires a resource.
4. Opening events are dispatched before publication/Active. A callback processing those events
   must not await the activation or dispatch fence whose completion depends on that callback.
   Existing `BranchOpening` remains the owner of required opening receipts.
5. A live subscription does not replay durable history. A stateful part reconstructs its derived
   state from the existing Timeline, serializes that reconstruction with live consumption and
   avoids applying the same local sequence twice. Historical folding does not repeat external
   side effects. An in-memory cursor or bridge handle is not a durable identity.
6. Input written from a callback is committed to the Timeline tail and consumed later. The callback
   cannot wait for its own subsequent dispatch. It must not treat the input receipt as completion
   of the requested action or substitute S2's after-publication receipt contract.

These are native flow constraints. The opening/activation cycle is a control-flow inference from
the existing source, not a reproduced C5 runtime failure.

## Confirmation and implementation gates

| Gate | Required result |
| --- | --- |
| Architecture confirmation | Confirm the complete consumer paths, declaration-based placement, original-row bind, named references and contracts together, including the single proposed generic public facade read. This document does not make them accepted APIs. |
| C4 | Complete full #63/#168 implementation review and merge before C5 implementation. |
| Original-row cleanup | Verify existing effect ordering closes admission, settles accepted callbacks/tasks and removes subscriptions before foreign unload. Do not return success merely because a reference was cancelled. |
| Startup and recovery | Bind may register subscriptions and reconstruct local state. Background business work starts only after activated=true; closing ends it. During a row reload, its own reconstruction/live-consumption sequence uses a fixed committed cutoff and local cursor, as native owners do. No shared replay queue or lock is introduced. |
| Acceptance | Implement the confirmed binding and pass the real cross-language flows below. Documentation CI is not this acceptance. |

The native owners remain authoritative. Opening receipts retain their existing native contract;
no new opening receipt protocol is part of this subset. An unimplemented Goal, Hook or other
plugin design does not supply additional bindings or change the native ordering rules.

## Verification required for delivery

Use real Node parts with the existing App Server/loader, isolated storage and a disposable home.
Extend the nearest public flow tests rather than testing private binding helpers.

- A branch part consumes native events in order, registers its own work, and writes validated input
  through the native Branch facade. Assert the resulting durable input and observed model flow.
- Two branches and two Sessions share the runtime while retaining independent subscriptions,
  state and task ownership; cross-branch communication remains input-only and asynchronous.
- A Session tool/method part imports TreeCommands, writes branch input and reads the declared branch
  owner's service. Assert that the returned Goal-style through is its consumed fold, not a fold
  recomputed by the caller from committed history. Two public services and same-name services on
  different branches must coexist without a registry. Reload fails stale calls and a fresh lookup
  reaches the new generation; Pending/absent exports return the existing unavailable/reloading path.
- Native runtime flow tests prove the named read retains PublicService bounds, branch isolation,
  per-call provider lookup and absence errors; existing typed service reads remain covered. Keep
  compile-fail coverage for internal Timeline/StateCell access, including the new named method.
- Row reload removes the old subscription and settles owned work. Closing leaves no callback or
  task waiting for an activation/fence that can no longer occur. Callback errors follow the native
  Dispatcher failure path; later events do not continue as healthy processing.
- Opening, restore and reconstruction preserve the committed/published boundaries and do not
  repeat a side effect or lose/duplicate a live event at the reconstruction cutoff.
- Existing C4 tools, C1 methods, Session broadcasts, first-read sampling and Pending behavior remain
  covered by their existing flows. Run graph/check for actual assembly/dependency changes.

## Impact and rule check

No dependency, event format or persistence format changes. The generated graph gains the
Session-scoped `HostDispatch#treeCommands` provided by TimelineTree.

Ownership: App Server's `session_typescript.rs` owns classification, original-row
projection and scoped binding; `lib.rs` inserts rows in the existing branch group and catalogs the
named services. The capabilities crate's TimelineTree language boundary owns TreeCommands and
ForeignBranchService references. Both crates already depend on bridge. Runtime owns and verifies
the generic named public read; it gains no bridge dependency or Goal-specific type/handling.
Business state and work stay in the TypeScript row. Existing native types/method semantics remain;
the one proposed public read signature is identified above.

| Rule | Application |
| --- | --- |
| §4 rule 1 | A part requests another owner's action through its existing input/command path; it does not mutate that owner's state. |
| §4 rule 2 | Adopted input stays with its consumer; the binding adds no revocation or transfer. |
| §4 rule 3 | No before callback is defined. |
| §4 rule 4 | Native Timeline/Dispatcher semantics remain independent of plugin business behavior. |
| §4 rule 5 | Existing batches and per-event order are preserved; no bridge batch acknowledgement is invented. |
| §4 rule 6 | Model context changes only through adopted inputs and tool results. |
| §4 rule 7 | Proposed named references call existing public services; no private state or another branch's Dispatcher. |
| §4 rule 8 | Reuse current assembly/Timeline/Dispatcher/rutis. The proposed named public read comes from #4/#64; its implementation/verification would stay within runtime. Rationale and scope are below. |
| §4 rule 9 | No event type, notification fact or persistence format is added. |
| §1 | Business state and tasks belong to the branch part; App Server owns assembly and foreign transport. |
| ADR 002 D1 | Branch behavior follows the branch lifetime. Session and process state stay with their owners. |
| ADR 002 D2 | No forwarding, subscription aggregation or registration Capability. Bind on the original row. |
| ADR 002 D3 | Dependencies point to this branch or enclosing scopes; native core does not depend on an optional foreign part. |
| ADR 002 D4 | Structural placement uses loader rows; business settings do not change assembly. |
| ADR 002 D5 | The proposed declaration, method modes, native payloads, error data and reference cancellation are language-neutral and require confirmation before implementation. |

## Requirements raised to mechanism owners

No new issue or independent mechanism proposal. The following requirement stays in the existing
#4/#64 runtime-services-by-name task and needs architecture confirmation before implementation.

| Item | Requirement and evidence |
| --- | --- |
| Concrete consumer | Goal #105 §1/§6/Appendix A.3/G: a Session part reads the branch-owned goal service through the public branch facade; reading committed history in the caller is not the owner's consumed fold. |
| Current boundary | Native Branch::service is generic over a Rust public service type and uses one typed branch key. It does not select one of multiple foreign services by its declared name. |
| Existing mechanisms considered | Ordinary TreeCommands injection supplies Session access, but its BranchRef still needs the public read. Session ctx.use of a branch dependency violates scope; caller-side folding violates ownership; a Session registration map adds wiring. Existing scoped keyed services and bridge references already supply storage/lifetime/invocation and need no replacement. |
| Generic addition | One Branch::named_service<T: BranchService + PublicService>(name), using the existing branch-qualified named key. No raw context/key access, registry, wait, cache or business type. |
| Ordering and lifecycle | Read-only per-call lookup. Existing absent-provider error and retained-handle semantics remain; no change to sequence, batches, publication, interruption, Pending or generation ownership. |
| Owner and verification | Runtime implements and verifies the generic public read with native flow/marker-bound coverage. C5's capabilities/App Server boundary projects foreign services and verifies the real Goal-shaped Session-to-branch flow. Both remain in #64; no new feature request. |

All other bindings use the existing scoped factories/catalog keys, subscriptions, history reads,
bridge references and effects. This proposal does not authorize another facade or core extension.

## Implementation

| Part | Where |
| --- | --- |
| `Branch::named_service` and `BranchScope::named_key` | [branch.rs](../../crates/dim-agent-runtime/src/branch.rs), [scope.rs](../../crates/dim-agent-runtime/src/scope.rs); the compile-fail coverage includes the named read of an internal service |
| `treeCommands`, BranchRef and `ForeignBranchService` | [tree_binding.rs](../../crates/dim-agent-capabilities/src/tree_binding.rs), provided by TimelineTreePlugin |
| Row classification, branch-group placement, original-row bind and publication | [session_typescript.rs](../../crates/dim-app-server/src/session_typescript.rs), [lib.rs](../../crates/dim-app-server/src/lib.rs) |
| Real Node flows | [branch_typescript.rs](../../crates/dim-app-server/tests/transport_flow/branch_typescript.rs) with the [branch part](../../runtime/fixtures/branch-part.ts) and [Session part](../../runtime/fixtures/branch-tree.ts) fixtures |

Choices made against the code:

- A business-service name is rejected when it is empty, contains `:` (a Session family prefix) or
  is already a catalog name; a branch row declares exactly one `branchPart`.
- `bind` must resolve to `null`; anything else, a throw or cancellation fails the row through its
  loading path, and none of its public services is published.
- The flows cover ordered consumption with reconstruction at a fixed cutoff, owned input written
  after `activated`, the Goal-shaped Session read of the owner's fold, two public services, two
  branches and two Sessions, a row reload that repeats no side effect, a throwing handler failing
  the Dispatcher, a failed bind, absent services (`reloading`), unknown branches and invalid
  declarations. The existing C4 flows run unchanged.

## Source references

- [Dispatcher](../../crates/dim-agent-runtime/src/core/dispatcher.rs)
- [Branch facade](../../crates/dim-agent-runtime/src/branch.rs) and [Timeline](../../crates/dim-agent-runtime/src/core/timeline.rs)
- [Scoped keys and PublicService](../../crates/dim-agent-runtime/src/scope.rs)
- [Branch assembly](../../crates/dim-agent-runtime/src/assembly.rs) and [TimelineContext](../../crates/dim-agent-runtime/src/timeline_context.rs)
- [TimelineTree / TreeCommands](../../crates/dim-agent-capabilities/src/timeline_tree.rs)
- [InputQueue reconstruction](../../crates/dim-agent-capabilities/src/input_queue.rs) and [Permission reconstruction](../../crates/dim-agent-capabilities/src/permission.rs)
- [Existing C4 resolver and original-row binding](../../crates/dim-app-server/src/session_typescript.rs)
- [Goal consumer contracts](plugins/Goal.md)
- [Time bus](../architecture/rust-agent/time-bus.md), [design rules](../architecture/rust-agent/principles.md) and [ADR 002](../adr/002-plugin-scope.md)
