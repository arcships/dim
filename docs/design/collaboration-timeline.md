# Owner-independent Timeline instances

Status: Proposed; not implemented. Mechanism proposal tracked in
[#335](https://github.com/arcships/dim/pull/335). Architecture and storage approval
are required before runtime implementation. Existing Collaboration, Workflow and
Agent contracts remain authoritative.

## 1. Purpose and limits

A branch Timeline assigns sequences, commits atomic batches and publishes events
to its ordered Dispatcher. State machines consume those events and write the
facts needed by their later decisions
([time bus](../architecture/rust-agent/time-bus.md)).

This proposal makes that mechanism instantiable without an Agent Session: one
Timeline/Dispatcher/store implementation, with separately owned roots and
lifetimes. Collaboration is the prospective first consumer, not a core instance
kind. Its existing `CollaborationTimeline` capability remains a business
capability; the new mechanism does not reuse that name.

An independent time bus is useful when a business owner needs an ordered,
recoverable sequence of work that outlives its participating Agent Sessions.
This is an architectural choice, not a prerequisite for persisting any process
state. Provider entries, Settings and other process records can continue using
their owners' typed operations (principles §4 rule 9). A lost in-memory Team
relation alone does not prove that a new bus is necessary.

| Existing mechanism | What it already provides | Relevant boundary |
| --- | --- | --- |
| Leader branch Timeline | Ordered Collaboration request, delivery and result receipts | Lifetime and history belong to that Agent Session |
| Process-owned records and typed services | Durable state independent of a Session | Do not themselves supply a Dispatcher-consumed sequence of work |
| `sessions.input` / `sessions.lookup` | Published Agent input and lookup of its submission correlation | Operate on Agent Sessions; do not create an independent owner |
| Existing notifications and subscriptions | Observe lifecycle and configuration owners | Notifications are not durable business history |

A no-Loop Session is not an existing assembly option: branch publication requires
LoopState, and Agent branch creation adopts a first input. Changing that contract
would also require a mechanism decision; a fake model is not a neutral owner.

Workflow and ExternalAgent need not migrate merely to supply a second consumer.
Their current storage and Session interfaces remain valid.

## 2. Shared mechanism, separate ownership

```text
process loader (one root, one rutis event bus)
├── agent instance
│   └── Session instance
│       └── agent branch: Timeline → Dispatcher → agent capabilities
└── timeline instance
    └── main: Timeline → Dispatcher → its owner's state machines

business owner ── target owner's typed input/command ──► other owner
external caller ── existing host authorization boundary ──► business owner
```

The first delivery creates one `main` bus per independent root. Generalizing
non-Agent branch creation, inheritance and tree commands is deferred until a
consumer requires it and its event contract is approved. Agent branching
continues through the existing TimelineTree. An independent root has no model,
Loop, InputQueue, prompt, tools or model context.

| Shared contract | Independent per root |
| --- | --- |
| Timeline sequences, atomic batches, synchronous `before`, commit then publish | Root identity, event rows and publication position |
| Dispatcher key filtering and ordered consumption | Subscribers, work, failure state and generation |
| Event envelope, encoding and storage implementation | Root metadata and branch directory |
| History, local and trace read semantics | History and lifecycle; no cascade to Agent Sessions |

Only a bus's sequence orders its events; no cross-bus or wall-clock order is
introduced. A Dispatcher handler registers its owner's work and returns
(AGENTS rule 3; principles §5). Subscribers rebuild through a fixed committed
cutoff, then consume later events without replaying side effects. A new generation
settles the previous generation's unfinished work before accepting new work.

## 3. Inputs, facts and boundaries

A capability writes through the `Timeline` service of its own scope. To affect
another owner, it uses that owner's typed input or command service. Agent inputs
continue through `Session::input` / `Branch::input` and the existing public
bindings; this proposal adds no process-wide arbitrary-event append entry and
does not replace Agent facades.

A business owner validates external shape and authorization at its entry and
produces valid events. Consumers use that contract directly; they do not
reinterpret malformed inputs or repeat boundary validation. Trusted in-process
plugins are not sandboxed by dependency declarations. Possessing a root address
does not authorize an external caller.

Independent-root business facts use non-reserved `EventKind::Extension` domains.
Core already has typed `collaboration/*` facts and reserves that domain; they
remain under their existing contracts. This proposal neither reclassifies them
as extensions nor makes Agent `branch/*` / `stop/interrupt` events valid generic
lifecycle commands. No event whitelist or business registry is added.

For Timeline-driven work, initiation and required terminal facts stay on the
initiator's bus, even when execution happens elsewhere. The receiving owner
writes its own results. Process configuration and external boundary producers
need not invent a source Timeline merely to submit a command.

Core Agents, the loader and other mechanism owners do not acquire a dependency
on optional Collaboration. The business capability observes existing owner
notifications or public history and records the facts its own decisions require,
within its own module; no separate wiring plugin is introduced.

A successful input reply means publication, not adoption or completed work.
A branch accepts each `submissionId` once, so resubmitting an input with the
same id after a lost reply writes nothing new; a lost reply does not prove that
a write failed. The initiating owner follows its command's settlement contract,
records uncertainty where needed and does not replay other uncertain writes.
This proposal adds no generic idempotency key, exactly-once guarantee or retry
manager.

## 4. Assembly and lifecycle owner

The proposed process service `timelineInstances` owns independent-root storage,
loader instances and outstanding lifecycle operations. It is a process plugin in
the existing loader, not another loader root. The business process owner supplies
the root's configuration and assembly profile. Its process plugins register the
parts that an independent instance mounts, following ADR 006 D3's ownership
model; extending registration to that scope requires the approval in §5. A part
generator captures configuration and identity, not live foreign service handles.

| Operation | Contract |
| --- | --- |
| Create | Atomically reserve a distinct root identity and its empty `main` bus; return that identity after storage succeeds |
| Load | Open an existing root, assemble its capabilities, rebuild their state and publish activation before background work begins; refuse a closed root |
| Unload | Stop admission for that generation, settle its accepted work and required publications, then remove services; preserve history for a later explicit load |
| Close | Stop admission, settle work and terminal facts, persist the closed state, then detach; do not close participating Agent Sessions |
| Delete | Require a closed, detached root; atomically remove only its owned records |
| List/read | Read metadata or committed history without loading capabilities or executing business work |

Unknown identity, invalid persisted data, unavailable/closing generation and
storage failure remain explicit errors; a failed close does not report success
or justify deletion. A lost create reply is not permission to recreate blindly:
the owner can list its stored roots. A runtime handle identifies one generation,
not a durable root.

Startup enumeration belongs to the business process owner: it reads its durable
root/configuration records and requests load. A root cannot bootstrap itself.
An unloaded write fails as unavailable; there is no implicit load, hot routing
or automatic idle policy. Work and I/O resources are registered for cleanup as
they are acquired. Closing/unloading settles accepted operations before removing
their services, including on assembly failure.

The proposed independent scope provides the Timeline mechanism, Dispatcher,
identity and lifecycle through named, declared dependencies. Their catalog
placement is part of the scope amendment below; existing branch-group names do
not become visible outside their group. Lifecycle waits distinguish activation from
closing. SQL and memory backends follow the same ownership and read/write
contract; an embedding without SQL does not gain a database requirement.

## 5. Required runtime adaptation

The current implementation cannot be reused unchanged merely by changing an id.

| Current owner | Required change |
| --- | --- |
| `TimelineKey` / `TimelineStore` | Replace Session-only root ownership with a common root identity; keep one event/branch/trace/snapshot store implementation |
| `TimelineFactory` / `BranchAssembly` / part registry | Separate neutral bus assembly from SessionConfig and Agent lifecycle; the independent instance mounts its owner's registered parts under its own profile, without fake Agent services |
| `TimelineContext` | Separate ordered dispatch and storage failure handling from LoopState, model/input processing and Agent stop behavior |
| Timeline / Dispatcher | Reuse sequence, commit/publication and dispatch algorithms; construct them with the new root's storage and failure owner |
| TimelineTree | Keep Agent creation, first prompt adoption and `branch/create` / `branch/start` receipts unchanged; no claim that a no-Loop root can call these methods |
| Process assembly | Register an instanced `timeline` group alongside `agent` in the same loader, owned by `timelineInstances` |
| Named services / C5 | Define explicit standalone-root identity and placement when binding it; today's `timelineIdentity` contains a Session id and branch service injection implies an Agent branch |
| Readers / Session broadcast | Read independent history under its own root; Agent notifications and existing typed Collaboration receipts remain on their current path |

The design proposes a bounded extension to the accepted scope rules:

| Foundation | Proposed amendment, requiring approval |
| --- | --- |
| Principles §1–§2; ADR 002 D1 | Permit capabilities on an independent Timeline scope as well as Agent Session/branch scopes; state still belongs to exactly one owner |
| ADR 006 D2–D4; plugin contract §2–§3 | Approve the proposed `timeline` instance group, its registered-part scope and named identity/lifecycle bindings together. The accepted tree currently has only `agent`, `session` and `branch`; this proposal does not add an implemented group or let branch services fall back into another group. The implementation design must define the new catalog placement consistently with D4 before code changes |
| Single-tree assembly | Permit the independent instance group within the existing loader; no second root or event bus |
| ADR 002 D5 | Expose lifecycle and any supported bindings as named, language-neutral contracts; do not assume existing C5 placement supports the new scope |
| Storage ownership | Introduce owner-neutral root records without making independent roots into Agent Sessions |

These are proposals, not changes already authorized by the foundations. Agent
facades retain their observable contracts even though their host implementation
must be adapted. No rutis extension is requested. The coordinated
[rutis 0.16 upgrade](rutis-0.16-upgrade.md) is implemented; that does not authorize
the new Dim scope or catalog placement proposed here.

### Storage compatibility

Current native storage is v25. It explicitly upgrades admitted v24/v23/v22/v21/v20/v19/v18/v17/v16/v15/v13
layouts; it does not reject every older database
([storage format](appserver/native-storage-format.md)). Branch foreign keys and
root enumeration currently assume `agent_sessions` and a required SessionConfig.

The target is one root catalog and common bus tables, with Agent configuration
only on Agent roots. Concrete schema and the next version number require a
storage-format decision before implementation; this proposal does not reserve
the next format version (v26 or later) or introduce a parallel Collaboration store.

That decision must preserve existing Agent identities, sequences, cuts, event
bytes, files and snapshots. Supported old layouts must upgrade transactionally
under an explicit admission policy; failures roll back and unsupported layouts
remain intact. Older binaries must reject a new incompatible format before
writing. Verification uses disposable databases, never a user's product home.

## 6. First consumer and delivery

Collaboration's independent owner would retain its own relations and work
history. Its business redesign must decide which process-memory relations and
temporary-Agent ownership move there. It must preserve the leader's current
request/delivered/result receipts used by clients. Provider-owned members remain
with ExternalAgent under the accepted provider contract; the bus does not
duplicate provider lifecycle state or invent Team nesting/authorization rules.

This mechanism proposal does not migrate Collaboration or mark that business
implemented. It does not require Workflow migration.

Delivery order:

1. Separate neutral bus driver/store construction from Agent behavior,
   preserving the existing Agent acceptance flows.
2. After scope and storage decisions, add the independent root and `main` bus,
   its lifecycle owner and focused acceptance.
3. Approve the first consumer's business design and verify its end-to-end flow.

Client protocol work is required only when clients are to read independent
history. v1–v3 do not carry Extension events; this proposal does not send them to
those clients. Keeping existing leader receipts avoids making a new client
protocol a blanket prerequisite for the internal mechanism.

Acceptance for runtime implementation covers committed order and batch
publication; crash before commit, after commit before publication and after
publication; no side-effect replay on reload; settlement before service removal;
failure and close/delete isolation between roots; SQL/memory equivalence; and
Agent storage/branch/input/broadcast regression flows. Storage compatibility is
verified against the explicitly admitted formats. Existing tests are extended
where the contract changes; no unrelated test framework is required.

## 7. Rule check

| Rule | Application |
| --- | --- |
| Principles §1–§2 | Standalone capability scope is an explicit proposed amendment (§5); not claimed to exist already |
| §4.1 commands/results | The owning state machine produces its results; other owners use typed inputs |
| §4.2 hand-over | Initiator work and terminal facts stay on its bus; uncertainty is settled without blind retry |
| §4.3 `before` | Synchronous in-process transformation before the atomic commit; shape and authorization checks remain at the entry |
| §4.4 core neutrality | No Collaboration-specific root kind, event handling or dependency in core |
| §4.5 batches | Atomic commit, publish afterwards, ordered per-event dispatch; no cross-bus atomicity |
| §4.6 context | Agent context changes only through its existing adopted inputs and tool results |
| §4.7 public access | Declared owner services and subscriptions; no private-state reads or consumer revalidation |
| §4.8 infrastructure | The requirement block below proposes the mechanism change before any implementation |
| §4.9 necessary facts | Only facts needed for later decisions; process records and UI notices keep their existing paths |
| ADR 002 D1 | One state owner per scope; standalone scope requires approval |
| ADR 002 D2 | Business observes dependencies in its own module; no wiring plugin |
| ADR 002 D3 | Optional business does not become a core dependency |
| ADR 002 D4 | Structural rows contain assembly/configuration identity; business configuration remains with its owner |
| ADR 002 D5 | Named, language-neutral services; standalone placement must be explicit |

**Requirements raised to mechanism owners (principles §4 rule 8):**

- **Blocked requirement:** an owner needs a recoverable, ordered work sequence
  independent of any participating Agent Session. No existing scope provides
  that instance. Persistence alone does not establish this requirement.
- **Existing mechanisms considered:** leader Timeline, process-owned records,
  Session input/lookup and notifications (§1). Keep them where their boundaries
  suffice; the first consumer must justify needing independent causal history.
- **Generic capability:** instantiate the existing Timeline/Dispatcher/store
  without Agent Loop/InputQueue, with an owned root and lifecycle (§2–§5).
- **Ordering, batches, interruption and reload:** per-bus ordering and atomic
  commit/publication remain; business cancellation belongs to its initiator;
  rebuild does not replay side effects; close settles accepted work.
- **Owner and tracking:** runtime and native-storage owners;
  [#335](https://github.com/arcships/dim/pull/335) tracks this proposed decision.
  Implementation is gated on its approved scope and storage-format decision.

## 8. Ablation review

The shared bus algorithm is retained because independent owners need the same
ordering, commit/publication and dispatch guarantees. Separate root metadata
and lifecycle are necessary to avoid borrowing an Agent's lifetime. The proposal
adds no universal event input, writer permission registry, generic deduplication,
automatic loader, idle worker, new client protocol or second consumer migration.
Non-Agent tree generalization is deferred rather than simulated with model
prompts or implemented as a parallel tree.
