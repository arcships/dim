# Native Session broadcast for TypeScript parts

Status: **Proposed; not approved or implemented.** Refs [C4 #63](https://github.com/arcships/dim/issues/63).

This author-proposed candidate defines one-way reception of a TypeScript part's own Session Event
facts. C4 requires a reception path; this particular Config/transport construction is optional
and is not a prerequisite for reviewing or merging the tool/method subset in #150.
The original-module Config convention, activation behavior and native transport ownership require
maintainer confirmation before implementation. Full C4 acceptance remains pending.

The [Session TypeScript design](session-typescript.md) defines the tool/method bindings separately;
their implementation is under review in [#150](https://github.com/arcships/dim/pull/150).
Names such as BoundRow and SessionNodeRuntime below identify that C4 assembly's owners, not
public APIs available on main. This proposal does not supersede the accepted binding document.

## Scope and ownership

The original Session row observes its own Session and owns its subscriptions, transport peer,
credentials and module effects. The shared Node runtime owns the shared listener. Native Session
and branch execution retain their existing owners, dependency rules and Pending behavior.
No process-wide Session subscription or arbitrary TypeScript event publication is introduced.
A trusted TypeScript part receives the complete native facts; this is not a filtered client feed
or a sandbox.

## Source and payload

The original row subscribes to `session_channel(SessionIdentity.session_id)`. Its
`SessionBroadcastEvent::Event { branch_id, event: Arc<AgentEvent> }` already supplies the complete
data. At the Rust-to-Node boundary, build `{branchId, event}` with the existing
[`AgentEvent` / `EventKind` Serialize implementation](../../crates/dim-agent-types/src/events.rs);
workspace serde already supports `Arc`. The SDK `NodeEvent.args` is `[{branchId, event}]`, so the
Cordis handler receives one record. The source channel and row's Link select its Session.

Preserve every Event kind and its native fields, sequence and safe-integer contract. Do not
substitute Rpc's filtered `ProductEvent`, rewrite controls/model/prompt facts, or convert to the
legacy v1 union. Existing [wire](../../crates/dim-agent-types/tests/wire_contract.rs),
[control](../../crates/dim-agent-types/tests/control_contract.rs) and
[branch](../../crates/dim-agent-types/tests/branch_contract.rs) fixtures stay unchanged. Serialization
errors follow the listener's existing error path. No new owner codec, record type or core API is
needed to serialize an already defined type at this external boundary.

`Fence` remains host observation machinery. A row inside the Session ends through closing and
disposal and is not guaranteed to receive `Closed`; an end-record observer must outlive the
Session ([event-driven flows §6](../architecture/rust-agent/event-driven-flows.md#6-session-广播)).
Do not translate this into Rpc `session.closed`, whose owner also waits for trace/resource
settlement. C6's Notice payload and arbitrary TS publication are separate contracts.

## Original-module Config

The proposed opt-in is `config.nativeEvents: null` on an ordinary loader row. Absence requests no
binding or transport resources. This is a new structural Config convention to confirm, not an
existing RowSchema/metadata declaration or an inference from `provides`.

```json
{
  "id": "session-observer",
  "name": "/absolute/session-observer/index.ts",
  "config": {"nativeEvents": null},
  "provides": {}
}
```

The original module explicitly admits this optional field in its Config schema: `null` for the
request and the following Link object for execution. App Server accepts only an absent or null
external marker; it derives the object during that original row's apply and replaces only the
marker before the original factory runs. The original factory/schema checks and Cordis
`plugin.Config` validate the realized config. Other members and original inject/provides
contracts retain their owners; unknown fields are not stripped or admitted implicitly.

| Realized `nativeEvents` field | Existing SDK contract | Producer / consumer |
| --- | --- | --- |
| `peer` | `Link.peer`, expected remote endpoint | Shared host listener identity; consumed by Link and Events.peer |
| `id` | `Link.id`, this Node endpoint | A fresh SDK PeerId per apply, using the existing UUID facility; also this row's native event name |
| `dial` | `Link.dial` | Host's actual loopback listener endpoint, consumed by Link |
| `token` | `Link.token` | Fresh credential accepted by this row's named SDK identity; consumed by the existing token handshake |

These are structural runtime inputs, never Settings, Timeline data, logs or replies. Loader/fiber
identity remains distinct from the transport peer: reapply uses a fresh peer id even when the row
identity stays the same. Queued events retain their old key and cannot select a new generation's
listener through a reused name. No separate event-name field, generation counter or peer-id list
is added.

The module starts native Cordis Link and Events in its own row, with local isolation of
`rutisPeer.<peer>` / `rutisLink.<peer>` services. `Events.in: [nativeEvents.id]` and
`ctx.on(nativeEvents.id, handler)` use the same unique name. Service isolation alone does not
filter the shared Cordis event bus. The small `definePlugin` context has no event API; this binding
uses a native Cordis module.

The pinned RuntimeResolver JsFactory permits deferred `inner.build`: its inject keys match the
resulting row, and its native config/instance validators add nothing beyond the defaults. BoundRow
can retain that factory/config and realize it in apply. Node still validates the original Config
in `rows.load`. This does not authorize deferring meaningful validators of arbitrary factories.

This construction targets initial apply and full reapply; volatile updates are not covered.
Pinned loader `JsRow::start` forwards `VolatileUpdate.config` directly to `Process::update_row`;
Node `updateRow` commits volatile references or restarts its Cordis row, bypassing BoundRow apply.
A raw null marker would overwrite the realized Link. The C4 embedding entry in implementation
[#150](https://github.com/arcships/dim/pull/150) uses `reconcile(..., None)` without an editable layer. That is the current entry's boundary, not proof
of mutable-row support. This proposal preserves that update path and records the limitation;
it adds no interceptor or second config authority. PluginManager remains responsible for editable
intent; mutable opted-in rows are outside this proposal's acceptance claim.

## Existing transport handoff and owners

```text
SessionBroadcastEvent on session/<Session id>
  -> original row: synchronous phase/cancellation check and existing serde
  -> emit(node_event(Link.id), NodeEvent { args: [{branchId, event}] })
  -> existing SDK EventsPlugin on this row's unique key, same rutis bus
  -> peer events.forward -> Cordis Events.in: [Link.id]
```

`NodeEvent` and `node_event(name)` are existing SDK transport definitions. The Session callback
projects and emits synchronously; SDK's per-key ordered chain owns network/handler waits. This
obeys the Session listener's no-I/O-wait contract in event-driven flows §6 without a product
forwarding service, wiring row, custom queue/worker or second bus. Forwarding the canonical Session
name independently through several peers duplicates delivery in shared Cordis; each link instead
uses its unique id in both directions of this one-way path.

The shared loopback listener belongs to C4's proposed SessionNodeRuntime owner. Each original row owns its
named StaticIdentity, listening peer/link, SDK EventsPlugin and source subscription, registering
cleanup immediately. Each apply uses `StaticIdentity::new(host_peer).accept_token(token, row_peer)`;
its LinkConfig names that identity and the shared listener. The host endpoint is common, while
credentials and peer registration are row-owned.

Pinned `rutis-bridge` 0.10.0 supports this through `identity.rs` named identities, `link.rs` named
identity lookup and per-link Registration, and `registration.rs::Registrations` credential checks
against each live registration's identity and generation. A central credential registry or fixed
row whitelist is unnecessary. The synthetic probe used a fixed three-token identity and did not
verify this dynamic composition inside actual BoundRow.

The existing raw loader endpoint starts with Mount::default and does not serve events.forward;
the native peer path targets the shared Cordis context instead. Measured transport cost is one
shared listener, one native peer/link and one Node I/O worker per receiving row, alongside the
existing loader channel. There is no RSS, throughput or maximum Session-count guarantee. The
PeerId-alias probe established two rows receiving one copy each, second-source isolation and
independent unload. It awaited peer calls directly; it did not verify the NodeEvent handoff,
real Session activation, Config realization or dynamic identities. Those are product acceptance.

## Apply and disposal

1. Obtain declared Session identity/lifecycle and shared runtime dependencies. Provide the row's
   named identity and start its listening peer/link with immediate cleanup registration. Do not
   wait for a connection before Node can dial it.
2. Build/apply the original JS factory with realized Config and register its cleanup immediately.
   Its native module starts Link and Events.
3. Wait cancellably for this row's peer Ready and events feature, then install SDK EventsPlugin
   with outbound `[Link.id]` and no inbound names. Await its existing fiber registration and
   register the typed Session listener before apply returns. Another optional row stays independent.
4. The source callback uses its captured row context/token and synchronously checks Session phase
   Active before emitting. Loading/closing/cancelled callbacks return without delivery. It does
   not await activated() inside the Session chain. Active is checked at callback execution, not
   at source emission: a fact emitted while Loading may be queued by rutis and forwarded when
   its captured listener executes after activation. Facts without a registered listener are not
   replayed. This candidate makes no emission-time cutoff promise and adds no buffer or lifecycle
   gate to obtain one. Failed Sessions never activate. This delivery boundary requires confirmation;
   autonomous startup work is outside this binding.

The wrapper retains the original inject keys and declares every external SDK dependency; its
own named identity uses existing self access. Peer/readiness waits observe row cancellation and
Session closing. The callback's supplied Ctx belongs to the emitter and is not its cleanup owner.

Pinned rutis `bus.rs::emit` snapshots listeners synchronously and chains dispatch per exact key.
Session emit order therefore reaches each row's NodeEvent key in order, where SDK EventsPlugin
waits for each forward and settlement. Branch sequence stays unchanged; other keys do not await
that chain, though rows share Node's event loop and transport resources. Local acceptance is not
remote completion.

On disposal, stop new source handoffs, unregister the source and SDK subscription, then explicitly
close the captured peer connection with existing `Connection::close` before releasing foreign
resources. This synchronous close interrupts blocked transport, closes the call table and finishes
outstanding waits; it does not depend on an aborted Link task having been polled. Existing C1
drain still precedes foreign unload. The original module settles its own accepted work through
its effects; closing a link cannot revoke an already consumed fact or forcibly stop a TS handler.

Exact-key unsubscribe does not join captured snapshots. SDK EventsPlugin's Forward holds the old
Arc<Peer>, ignores callback Ctx and does not select row cancellation; a captured old forward must
fail against its closed connection, not select a new generation. LinkPlugin task abort alone is
not a completed close, and Connection::drain is not an outgoing-notification join. Actual BoundRow
unload/reload must verify connection closure, module effect settlement and stale-delivery rejection.
No extra tracker, lock, retry loop, cursor or lifecycle phase is prescribed.

## Review and acceptance

Confirm the Config convention, current immutable-entry limitation, startup-delivery behavior and
transport/ownership integration before implementation. The complete Event payload retains the
existing source contract. Product verification then uses protected serve/Rpc, the original module
schema, two receiving rows in one Session and another Session in the same Node process. It must
cover activation/failed initialization, a source emitted before activation whose listener is
delayed until after activation, absent/disabled opt-in, one copy per delivered emission,
ordering, dynamic identities, in-flight unload, same-row reload, stale peers, and continued native
tool/C1 execution. Synthetic transport success does not fulfill this acceptance.

[S8b §4.5](appserver/settings-service.md#45-events) process settings/<source> and settings/projects
are different typed sources, owned by SettingsChanged and ProjectAuthorized. Their approved
serialization is not implemented, and Link does not declare source selection. They remain S8b
owner work; until binding exists, TS owners read at use time as its design specifies. This Session
binding adds no generic source map or Settings watch channel. Full C4 stays incomplete until its
Session fact reception passes the actual flow; C6 Notice remains separate.

## Impact

| Owner or boundary | Proposed effect |
| --- | --- |
| C4 App Server structural input | Accept the explicit nativeEvents marker and realize existing Link fields inside the original scoped row; keep the original module schema and dependency declarations. |
| Original Session row | Own the typed source subscription, named identity, native link, SDK EventsPlugin and connection close through its existing effects. |
| Shared Node runtime | Own the shared listener; retain the existing raw loader/service channel. |
| Original Cordis module | Admit the realized Config, select its unique inbound name and settle its accepted work on disposal. |
| Session broadcast and event types | Observe the existing typed channel and reuse AgentEvent Serialize; no new source event, codec, field conversion or persistence format. |
| Core, ToolRegistry and C1 | No new core API, readiness phase, tool behavior or method dispatch. C1 drain must still finish before foreign unload. |
| S8b and C6 | Keep their source selection, Settings encoding and Notice/Rpc contracts with their owners. |

The only new module-input convention is the optional nativeEvents Config slot. Its consumers
are the original row binding and that module's original Config/Link/Events setup. The four
realized fields reuse Link's existing contract. No dependency or generated graph changes belong
to this design-only change.

## Ablation

Within this candidate, the explicit Config slot distinguishes a requesting module and admit the host's
runtime inputs through its original schema. Link.id already supplies the unique event name;
a second event-name field or generation counter adds no requirement. The existing SDK NodeEvent
handoff keeps I/O waits off the canonical Session channel while retaining
per-key order. Identity, registration, connection close and effects remain with existing SDK and
row owners. No additional abstraction, forwarding capability, manager, lock or cursor is needed.

## Rule check

| Rule | Application |
| --- | --- |
| §4 rule 1 | This one-way binding does not change another owner's state or write result facts. A consuming feature still uses its owner's public command path. |
| §4 rule 2 | SDK channel acceptance and remote consumption remain distinct. Disposal cannot revoke a fact already consumed by the module. |
| §4 rule 3 | No before callback; the external Config boundary validates shape, while native event data uses its producer's existing Serialize contract. |
| §4 rule 4 | Timeline, Dispatcher and event types gain no TypeScript-specific branch or filtered payload. |
| §4 rule 5 | Observe facts after existing atomic commit and Dispatcher publication; preserve branch sequences and Session emit order without a new batch or tree position. |
| §4 rule 6 | Broadcast reception does not insert model context. A business feature still uses adopted inputs or tool results for model-readable content. |
| §4 rule 7 | Use the public typed Session broadcast, scoped identity/lifecycle and SDK services; no private business-state reads. |
| §4 rule 8 | Reuse current loader factories, native event keys, registrations, Connection::close and effects. No core or rutis capability is requested. |
| §4 rule 9 | Runtime Config, credentials and transport notifications do not enter Timeline; the existing committed Event facts remain authoritative. |
| §1 | The Session part owns its resources and declares external dependencies; apply registers cleanup immediately and business reception requires the existing Active phase. |
| ADR 002 D1 | Session observations and per-row peers belong to the original Session part; the listener is a shared process resource and branch facts stay branch-owned. |
| ADR 002 D2 | No wiring module, forwarding service or registration host; existing SDK transport features are composed inside their owners. |
| ADR 002 D3 | The optional part depends outward on existing Session/process services. Core has no hard dependency on it, and Pending behavior is unchanged. |
| ADR 002 D4 | The opt-in and Link fields are structural loader Config. Business settings retain source-local authority; mutable marker updates remain explicitly unsupported here. |
| ADR 002 D5 | The contract is the existing named event path, JSON argument and Link fields. Rust serde, EventKey and lifecycle access are its native binding; closing the owned connection ends native waits. |

## Requirements raised to mechanism owners

None. Existing mechanisms supply the typed source, serialization, transport handoff, identity
registration, ordering and disposal operations. The remaining Config and lifecycle questions
require confirmation and product acceptance within C4, not infrastructure expansion.
