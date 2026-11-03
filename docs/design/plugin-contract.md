# Plugin contract

Status: Accepted under [ADR 006](../adr/006-plugin-contract.md). Each section states the contract. The theory it builds on, the [time bus](../architecture/rust-agent/time-bus.md)
and the [design rules](../architecture/rust-agent/principles.md), is unchanged and applies now.
[中文](../zh/design/plugin-contract.md)

This document is Dim's architecture as it applies to plugins, in any language: what the instance
tree means, where a part is placed, the services and bus events Dim defines, Timeline facts and their
consumption, how bus events are consumed, Capability, and publication and closing. How a part is
mounted, names its services, crosses languages and uses the rutis lifecycle and bus is in
[How Dim uses rutis](rutis-usage.md), whose appendices are the Rust and TypeScript bindings. A binding
adds no semantics. When a binding seems to need a rule that neither document states, change the
documents first.

## Reading path

| You are | Read |
| --- | --- |
| Writing or reviewing any plugin | [Time bus](../architecture/rust-agent/time-bus.md), then §1–§8 here, then [How Dim uses rutis](rutis-usage.md) and its appendix for your language |
| Designing a plugin | Also [ADR 002](../adr/002-plugin-scope.md) (placement, configuration) and §6 (which rules usually apply) |
| Changing a mechanism (Timeline, Dispatcher, assembly, catalog) | Also [principles §4 rule 8](../architecture/rust-agent/principles.md) and §9 |
| Touching an existing part | Its page under [`plugins/`](../architecture/rust-agent/plugins/README.md) or [`design/plugins/`](plugins/README.md) |

The [single-tree](../architecture/rust-agent/single-tree.md) and [lifecycle](../architecture/rust-agent/lifecycle.md)
documents describe how the current Rust implementation realises this contract. They are not
required reading for writing a plugin.

## 1. Concepts

| Term | Meaning |
| --- | --- |
| Instance tree | The process root, and the Agent, Session and branch instances below it (§2) |
| Plugin | One feature: one loader row in the process, whose Session and branch parts it registers ([rutis usage §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)) |
| Part | What a plugin runs in one scope: the plugin itself in the process, and in each Session or branch the part that instance mounts from the plugin's registration |
| Scope | Where a part lives: the process, a Session or a branch (§2, §3). rutis realises each Session and branch as an instance of an instanced loader group, which mounts the parts registered for its scope ([rutis usage §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)) |
| Service | A named object that a part provides to other parts; its identity is a name and one scope ([rutis usage §3](rutis-usage.md#3-services-by-name)) |
| Catalog | The names Dim defines (§4) |
| Timeline | A branch's ordered, durable log of facts |
| Fact | A committed Timeline event: the `AgentEvent` envelope (`id`, `sequence`, a creation time, optional `tags`, and `type`, `name`, `data`) |
| Domain | An event's `type`. The core reserves its domains; an extension writes only its own (§5.1) |
| Command | A fact or input that asks the owner of some state to change it. The owner decides and writes the result |
| Dispatcher | The branch mechanism that delivers committed facts, in sequence order, to the parts registered with it |
| `before` | A synchronous transform of a draft fact inside the commit lock. Rust, in process only (§5.1) |
| Bus event | A rutis event on the root's flat bus ([rutis usage §5](rutis-usage.md#5-the-event-bus)); how Dim's parts consume bus events is §5.3. Not to be confused with a Session notice, one kind of payload on the Session broadcast |
| Generation | One load of a part, from `apply` to unload ([rutis usage §4](rutis-usage.md#4-lifecycle-rutis-provides)). A reload starts a new generation |
| Capability | A part that is a state machine on a branch Timeline (§6) |
| Owner | The part that writes a piece of state or a domain. Every state has exactly one |

## 2. The instance tree

```text
process (root)                 process parts: storage, catalogs, connections, Agents, Rpc …
└─ agent ⟨instanced⟩          the host's Agent facade; no feature parts
   └─ session ⟨instanced⟩     one Session: its Session parts and Session services
      └─ branch ⟨instanced⟩   one branch: its Timeline, Dispatcher and branch parts
```

| Scope (rutis group) | An instance opens | It closes | Mechanism services in the instance |
| --- | --- | --- | --- |
| Process (root) | Host start | Host shutdown | — (process parts provide their own) |
| Agent (`agent`) | Agents creates an Agent | Agent close, host shutdown | Host-internal |
| Session (`session`) | An Agent creates or restores a Session | Session close, idle unload | `sessionIdentity`, `sessionLifecycle`, `treeCommands` |
| Branch (`branch`) | TimelineTree creates a branch | The branch ends, or its Session closes | `timeline`, `dispatcher`, `branchLifecycle`, `timelineIdentity` |

Visibility follows the tree, as in [principles §2](../architecture/rust-agent/principles.md):

| Direction | How |
| --- | --- |
| Up (an enclosing instance, the process) | Inject the service by name |
| Down (Session to branch) | `treeCommands`: input by branch id, branch creation. Never by injecting a branch service |
| Sideways (sibling branches, other Sessions) | Not visible. Go through a common ancestor's business service, such as `sessions` |

The scope roots, the facades and their publication rules are mechanisms. Each Session and branch is
published only after the parts its assembly profile requires are Active
([single-tree](../architecture/rust-agent/single-tree.md#结构)).

*Today:* the tree, the scopes, the visibility rules and the mechanism services are implemented, and
the Session and branch mechanism services cross languages.

## 3. Placement

A part's placement is its scope: the process for the plugin itself, and the Session or branch that
mounts a registered part ([rutis usage §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)).
Which scope that is, is a Dim decision:

1. **Place by the owner of the state** ([ADR 002 D1](../adr/002-plugin-scope.md#d1-scope-follows-the-owner-of-the-state-and-its-lifecycle)).
   State shared across Sessions, or outliving them, goes in the process. State shared by a Session's
   branches goes in the Session. A branch's facts, its ordered consumption and turn-bound work go in
   the branch. A feature is one plugin: it registers a part only for a scope where it owns state, and
   one design documents the plugin with all its parts.
2. **Required or optional is the host's decision.** The assembly profile lists the parts a Session or
   branch waits for. A plugin added by configuration is optional: if its part is Pending or Failed,
   only its own features are affected.
3. **Structural and business configuration stay apart** ([ADR 002 D4](../adr/002-plugin-scope.md#d4-structural-configuration-and-business-configuration-are-separate)).
   Business settings live in the owner's source, read through `settings` when they are used. Changing
   them reloads nothing.

## 4. Dim's services

The names Dim defines, each following the identity and shape rules of
[rutis usage §3](rutis-usage.md#3-services-by-name). The authoritative table is the
[generated catalog](../architecture/rust-agent/generated/catalog.md): `xtask graph` writes it from the
real assembly with each name's group, provider, methods and whether it crosses languages (rutis usage
R4). This table says what each name Dim defines is for.

| Name | Scope | Provider | Crosses languages today | Notes |
| --- | --- | --- | --- | --- |
| `sessions` | process | Agents | yes | Start or drive another Session |
| `sessionPermissions` | process | Agents | yes | Marked `#[rutis::service]` |
| `sessionMetadata` | process | SessionStore | yes | SessionTitle is the title's one writer |
| `settings` | process | Settings | yes | Business configuration |
| `auxiliaryModel` | process | App Server | yes | Auxiliary model requests |
| `pluginFiles` | process | host | yes, when the host supplies a root | A process part's own files |
| `workflowFiles` | process | App Server | yes | Marked `#[rutis::service]` |
| `collaborationMembers:<kind>` | process | the member provider | designed, not implemented | [ExternalAgent](plugins/ExternalAgent.md); registers with Collaboration when built (rutis usage R6) |
| `treeCommands` | `session` | TimelineTree | yes | `input` by branch id, `create`, `list`, `info` |
| `sessionIdentity` | `session` | SessionScope | yes | `get()`: `{sessionId, restored}` |
| `sessionLifecycle` | `session` | SessionScope | yes | `activated`, `closing`, `hold`, `release`: a hold is an id the calling row owns, ended by `release(id)` or that row's unload |
| `sessionConfig` | `session` | SessionScope | yes | `get()`: the creation-time configuration, the snapshot a tool call carries |
| `timeline` | `branch` | BranchScope | yes, except `before` | `append`, the published sequence and the event query (§5.4); the query reaches other languages with its declaration |
| `dispatcher` | `branch` | BranchScope | yes, `consume`, `listen` and reads; their Rust forms `follow` and `subscribe` are Rust-only | Ordered consumption (§5.2) |
| `branchLifecycle` | `branch` | BranchScope | yes | `activated`, `closing` |
| `timelineIdentity` | `branch` | BranchScope | yes | `{sessionId, branchId}` |

The Rust host ports and registries (ToolRegistry, PromptRegistry, ToolHost, ModelHost,
PrepareModelHost and the others in the [generated services](../architecture/rust-agent/generated/services.md))
get names and scopes in phase 2. They stay Rust-only unless a foreign part needs them. So do
`BranchOpening` (opening receipts are Rust futures) and the facades' state reads (`StateCell<S>`,
`PublicService`), which are keyed by Rust type. A foreign part exposes its state through a service.

Tools, tool sources, prompt sources and method namespaces are not names. A part registers each with
its owner, and the registration ends when the part unloads
([rutis usage R6](rutis-usage.md#r6-contributions-register-with-their-owner)): `register` on
ToolRegistry or PromptRegistry, or on the method table of its scope, `sessionMethods` (`session`) or
`managementMethods` (process, served to protected-local v3 only), which Rpc reads per call. Tools
and method namespaces register from any language; prompt and tool sources stay Rust-only until a
foreign part needs one. All four register today.

Two mechanism contracts tie the facades to core parts: a branch is published only with a `LoopState`
provider, and a Session only with a `TimelineTreeControl` provider. Loop and TimelineTree are therefore
core parts, written in Rust. This design does not change that.

## 5. Events

### 5.1 Timeline facts

A part writes facts through its branch's `timeline` service. The Timeline checks only the envelope and
rejects an opaque append in a core domain (`RESERVED_DOMAINS` in `dim-agent-types`: `input`, `stop`,
`approval`, `permission`, `exec`, `turn`, `prompt`, `context`, `model`, `step`, `branch`,
`collaboration`, `session`). An extension writes its own domain, such as its plugin id; `type` and
`name` are non-empty and contain no `/` or `*` ([ADR 001](../adr/001-harness-foundation.md#events-and-ownership-issue-2)).
An extension event may also carry `context_render` and `context_state`, two optional booleans the
writer sets; the core neither derives nor overrides them. They select the row for the context read;
who consumes it is still open.

The rules for writing facts are not restated here. Commands to owners, handover, `before`, batches,
the model-context path and which facts belong in the Timeline are
[principles §4](../architecture/rust-agent/principles.md) rules 1–3, 5, 6 and 9, and AGENTS rule 10.
One consequence matters for other languages: a `before` transform runs inside the commit lock, where no
cross-process code may run ([ADR 001](../adr/001-harness-foundation.md#trust-and-cross-language-calls)),
so only in-process Rust registers one.

### 5.2 Consuming facts

A part consumes a branch's facts by registering with its `dispatcher` and naming the keys it consumes:
an exact `domain/name`, `domain/*` or `*`. The Dispatcher delivers them in sequence order, one fact at a
time, to every part registered in the current generation. A new generation rebuilds as the
[capability reload protocol](../architecture/rust-agent/single-tree.md#能力重载协议) describes.

A part that rebuilds registers once for both (R1). The registration fixes the cutoff `S`, hands its
handler the replay, the branch's own facts through `S` as `local` reads them and not filtered by the
keys, then each matching fact after `S`, in sequence order and none twice. Live facts wait until the
replay is handled, so the work it leaves, such as failing the operations a previous generation left
open, is done in the replay. Registering, reading the cutoff and rebuilding by hand stays valid.

### 5.3 Bus events

Bus events are rutis events on the root's flat bus; their delivery, payload and naming rules are
[rutis usage §5](rutis-usage.md#5-the-event-bus). Consuming a bus event is consumption too, with a weaker
delivery than facts: a bus event is not stored or replayed, and only the listeners registered when it is
emitted receive it. A bus event consumer therefore recovers the way [time bus §3](../architecture/rust-agent/time-bus.md)
asks of every state machine, from the Timeline or by re-reading the owner's state, so a missed bus event
can delay a decision but cannot change it. [SessionTitle](plugins/SessionTitle.md) is an example: each
`input/applied` event makes it re-read the title metadata and a fixed history prefix, so a missed event
only delays the title until the next adopted input.

The bus names the Dispatcher and the Timeline use internally are theirs; no part emits or listens
under them.

Public bus events:

| Name | Emitter | Payload | Meaning |
| --- | --- | --- | --- |
| `session/{sessionId}` | SessionBroadcast | A committed event, a Session notice, a fence, or closed | The Session's live projection for clients and observers ([Session broadcast](../architecture/rust-agent/event-driven-flows.md#6-session-广播)) |
| `settings/{source}` | Settings | `{source, scope, revision}` | A business source changed |
| `settings/projects` | App Server Settings | `{root, cwd}` | A project root was authorized |
| `skills/catalog` | SkillsCatalog | catalog version | The Skill catalog advanced |
| `agent/session-restored`, `agent/unloaded-session-deleted` | Agents | ids | Host-internal, consumed by Collaboration and Provider; [Session loading](appserver/session-deletion-and-fork.md#65-opening-the-fork) |

*Today:* the events are implemented as listed, and TypeScript rows listen to them directly.

### 5.4 The event query

A part reads facts through its branch's `timeline` service with one bounded query. The selection is:

| Element | What it says |
| --- | --- |
| Scope | This branch, or the branch chain: each ancestor up to its cut, root first, then this branch to its head |
| Kinds and flags | The event kinds it accepts (`domain/name`), and the envelope's context flags (§5.1) it selects on |
| Event id | An equality on the event id: one id selects that event, so a caller reads one event or looks up its position without paging |
| Predicate | One equality against one JSON scalar. The path is a sequence of field names inside the event's data joined by dots, with no array indexing and no wildcards |
| From | Where the read begins, named by an event id or by a sequence; the event it names is part of the read. A branch read takes it on that branch; a chain read begins in the segment it names and carries on, skipping the segments before it |
| Order | The key the reply is ordered by — the sequence — and the direction: ascending (oldest first) or descending (newest first). A chain read orders by chain position first |
| Page | How many matching events the read skips, and how many it returns; the second is nonzero by type, so no read is unbounded. Both count events, not storage rows |

The elements combine: an event is selected when it satisfies all of them, and flags named together
are alternatives, so an event is accepted when it carries at least one of them — the context read
selects the rows that carry `context_render` or `context_state`. A selection that names no kind
accepts every kind, one with no event id accepts every event, one that names no start begins at the
first event of its scope, and a flag it does not name constrains nothing: no omission is an error.

Paging is skipping: a caller repeats the query with a larger skip, and a page shorter than the
count is what says the selection is exhausted. A page does not align with an atomic batch — it may
end inside one and the next page carries the rest, which a consumer folding in order handles. A
read is taken at what is published when it runs, so a consumer that folds several pages drops what
it has already folded.

The path's spelling is fixed here and not in a binding: there is one form, the one above, and the
storage translates that form. A binding writes this path syntax and invents no other.

The selection is what the query is for. Its elements describe events, not a slice of the store, so
the store executes them where the events are kept and returns the page those events form. A read
never returns a fact above the coverage of the branch it reads: an ancestor to its cut, this branch
to what is published, which the service bounds and no caller states. A
selection the store can execute from an index costs the page; one it cannot — an equality on a field
it holds no index for, a flag set with no index of its own, or any selection in a store that filters
in memory — costs the scoped range and proves the end of the scope the same way. A
chain page reaches one segment after another, so it costs what it reaches. The reply carries each
event's chain position with it, so a caller reads where an event sits without asking again.

The query returns facts; it does not fold, project or aggregate them. What a part folds from facts
is its own state and stays with it ([ADR 002](../adr/002-plugin-scope.md) D1). A part that needs a
whole prefix reads pages and folds them itself.

| Input | Result |
| --- | --- |
| A kind or a data path the store does not know | No rows; not an error |

The query addresses the event contract, not the storage: the session, the branch, the kind, the
event id, the two context flags and paths inside the event's data. The reply carries each event's
position, but the request does not select on one. Table, column and
index names, the content table, batch metadata and every other part of the stored layout are not
addressable, and SQL is not a capability interface
([ADR 019](../adr/019-event-query.md) D3).

Besides the query, the service keeps two things: appending a fact of the part's own domain (§5.1)
and reading the published sequence. Branch segments are the branch directory and recovery
snapshots are derived state; neither is an event, and each keeps a typed read of its own.

A read shaped for one consumer is compatibility, not a contract: it exists only while that
consumer moves to this query, and it is removed after. A selection the query cannot express is a
decision to extend the query, never a method to add. While a consumer still uses its own read, the
two shapes must agree on ordering and on where a page ends, or the difference is stated
where that consumer's move is followed ([ADR 019](../adr/019-event-query.md) D2, D5).

The request and reply are serde data of the `timeline` name
([How Dim uses rutis §3.2](rutis-usage.md#32-shape)), so the spelling of a scope or an order
belongs to a binding: its Rust form is [appendix A](rutis-usage.md#appendix-a-rust-binding), and the
generated declarations are where the other languages read its shape. This section states the query,
not the rest of its spelling.

## 6. Capability

A Capability is a part that folds a branch's facts in sequence order, or writes facts of its own
domain: a state machine on a branch Timeline. It is defined in
[principles §1](../architecture/rust-agent/principles.md); it is not declared, and no mechanism enforces
it ([ADR 006 D6](../adr/006-plugin-contract.md#d6-capability-is-a-convention-a-state-machine-on-a-branch-timeline)).
A part that only provides services or only writes inputs is not a Capability; it is just a part.

Being a Capability adds no rule of its own; it says which of the existing rules usually apply:

| Part | Existing rules that usually apply |
| --- | --- |
| A Capability | [Time bus](../architecture/rust-agent/time-bus.md) §1–§5; principles §4 rules 1–3, 5, 6, 9; AGENTS rules 3 and 8 |
| Every part | ADR 002 D1–D5; principles §4 rules 1, 2, 7 and 8 |

Examples:

- The Goal branch part folds facts and decides continuations, so it is a Capability.
- The Goal Session part provides `goal_update` and `session.goal.*` and writes change inputs by branch
  id. It holds no Goal state, so it is not a Capability.
- Questions provides `ask_user` and `session.question.*`. Its waiters are its own tool calls and it
  writes no facts, so it is not a Capability.

A design document's Rule check says which parts are Capabilities, then answers the existing list as
AGENTS.md asks: principles §4 rules 1–9, §1 and ADR 002 D1–D5. The table only shows which rules
usually apply; a rule that does not apply is answered with the reason.

## 7. Lifecycle of a part

### 7.1 Phases

Build, `apply` and unload are rutis's ([rutis usage §4](rutis-usage.md#4-lifecycle-rutis-provides)).
Dim adds two phases between them, for Session and branch parts:

| Phase | The part may | The mechanism does |
| --- | --- | --- |
| Activated | Start background work and input consumption | `activated()` resolves when the Session or branch is published; it is `false` if assembly failed |
| Closing | Stop admission, cancel its own work, release resources, write terminal facts | `closing()` resolves when close starts. A Session is closing from the moment its close begins |

Dispatcher handlers can receive opening facts before activation.

### 7.2 Generations, reload and rebuild

A reload is a restart from the Timeline (time bus §5). How the unloading generation settles its work,
and how the new generation subscribes, fixes its cutoff, rebuilds and fails the open work it finds, is
the [capability reload protocol](../architecture/rust-agent/single-tree.md#能力重载协议). It holds in
every language; R1 makes its subscribe-and-rebuild steps one Dispatcher registration.

## 8. Guarantees, conventions and the audit

The guarantees the mechanisms give are listed in [ADR 006 D7](../adr/006-plugin-contract.md#d7-what-the-mechanisms-guarantee-and-what-they-do-not).
The rules a design answers (§6) are conventions. Three things check them:

| Check | What it covers |
| --- | --- |
| `xtask graph` / `check` | Crate boundaries, generated assembly graphs, plugin pages against the code, and the [catalog table](../architecture/rust-agent/generated/catalog.md), which also marks which parts are Capabilities (they inject `dispatcher` or `timeline`) |
| Review and the pull request template | The Rule check: principles §4 rules 1–9, §1, ADR 002 D1–D5 |
| Flow tests | Observable behaviour: ordering, rebuild, reload settlement, close |

## 9. Requirements raised to mechanism owners

Each requirement comes from the mechanism's own responsibility and closes inside it
([principles §4 rule 8](../architecture/rust-agent/principles.md)). Issues are opened when this
design is merged. R4–R7 concern how Dim uses rutis and are in
[rutis usage §6](rutis-usage.md#6-requirements-raised-to-mechanism-owners).

### R1. Ordered consumption with rebuild as one Dispatcher registration

- **Blocked:** the subscribe-and-rebuild steps of the [capability reload protocol](../architecture/rust-agent/single-tree.md#能力重载协议). Every Capability repeats them by hand. Examples are
  `crates/dim-agent-capabilities/src/wait.rs:361-380` and `runtime/plugins/goal/branch.ts:120-128`.
  A mistake (a cutoff read before subscribing, `history` instead of `local`) silently loses or
  double-counts events.
- **Existing mechanisms considered:** `subscribe`/`listen` plus `committed`/`local` are correct but
  leave the protocol to every author. An SDK helper per language would replicate the protocol
  (ADR 006 D1).
- **Needed:** `dispatcher.consume(name, keys, handler)`, which registers, fixes `S`, delivers
  `local(..=S)` once as the replay, then delivers live events after `S`, through one handler and in
  sequence order. Live events wait until the replay is handled, as the hand-written rebuild gate
  holds them, so the failure terminals a rebuild writes come before them. It carries no business
  meaning.
- **Effect on sequence, batch, interruption, reload:** none on the Timeline. The delivery order equals
  the hand-written protocol's. Reload behaviour is that of §7.2.
- **Owner:** Dim runtime (Dispatcher). Delivered in phase 5
  ([#380](https://github.com/arcships/dim/issues/380)): `consume` in every language, `follow` in
  Rust; the replay is one delivery of the whole slice, not one call per event.

### R2. Lifecycle and identity services in the Session and branch scopes, for every language

- **Blocked:** AGENTS rule 1 (background work starts after publication). A TypeScript Session row has no `activated()` or `closing()`, although a Rust Session
  part has `SessionLifecycle` ([authoring guide](../typescript-plugin-authoring.md#plugin-shape-and-lifetime)).
  The row learns its Session id from the C4 setup rather than from a service.
- **Existing mechanisms considered:** C4's per-row setup carries the id but not the phases.
  `branchLifecycle` already exists for branch rows.
- **Needed:** `sessionLifecycle` (`activated`, `closing`, `hold`) and `sessionIdentity`
  (`{sessionId, restored}`) as catalog services in `session`, with serde shapes, next to the existing
  `branchLifecycle` and `timelineIdentity` in `branch`. `hold` returns a handle released by a call or
  by the row's unload; in the serde form the handle is a hold id, which `release(id)` ends, and the
  hold is a cleanup of the calling row. `sessionConfig` exposes the creation-time configuration the C4
  setup carried, so the setup goes. The process scope gets none: process parts follow the host's
  start and shutdown through the rutis lifecycle.
- **Effect:** none on the Timeline. It exposes the existing phases.
- **Owner:** Dim runtime (SessionScope) and App Server (catalog). Delivered in phase 4
  ([#376](https://github.com/arcships/dim/issues/376)).

### R3. Rpc maps service errors of contributed methods to wire errors

- **Blocked:** ADR 002 D5 (errors are data). `runtime/plugins/goal/session.ts:15` maps native error variants (`InvalidInput`,
  `UnknownBranch`, `Closed`, `Reloading`) to JSON-RPC `appError` codes. Every method provider that
  forwards a native error would have to repeat the mapping, and a new native code needs a change in
  every plugin.
- **Existing mechanisms considered:** the `MethodReply` error form `{code, appError, message}` puts the
  wire code into the plugin contract.
- **Needed:** contributed methods return `{code, message}` data, or rethrow the service error they
  received. Rpc maps native and documented codes to the wire `appError`, as it already does for built-in
  methods.
- **Effect:** none on the Timeline. Wire results stay the same.
- **Owner:** App Server (Rpc). It changes the TypeScript SDK shape, so it lands in phase 4. Delivered
  there ([#376](https://github.com/arcships/dim/issues/376)): `invalid_params` maps to JSON-RPC
  -32602, other codes to -32000, and a rethrown native error as Rpc maps it for built-in methods.

## 10. Migration

Each phase is a complete, reviewable change with its tests and documentation. Dim is unreleased, so no
phase keeps a compatibility path: no aliases, shims or transition shapes for anyone outside the
repository. A phase changes the SDK, the repository's plugins and the documents where it needs to.
This design changes no persisted or wire format.
R1–R3 are in §9 and R4–R7 in [rutis usage §6](rutis-usage.md#6-requirements-raised-to-mechanism-owners).

| Phase | Work | Acceptance |
| --- | --- | --- |
| P1 | The [rutis 0.16 upgrade](rutis-0.16-upgrade.md) | That design's flow checks and the full CI sequence |
| P2 | Catalog and Rust binding: register Dim's names and groups; a Rust part declares dependencies by name and provides at the same keys, each key taken from the catalog for the name and the copy's instances ([rutis usage §3.5](rutis-usage.md#35-how-each-runtime-resolves-a-name), [appendix A](rutis-usage.md#appendix-a-rust-binding)); the old types remain only until P3 has moved every part | A Rust part that imports no layer type mounts in process, `session` and `branch`, injecting and providing by name; a misplaced row fails at composition with the group it needs |
| P3 | Move every part to name dependencies, in batches: process and simple parts; core Session and branch parts; the rest. Then the part registry and the instances' mounting ([ADR 006 D3](../adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it), [rutis usage §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)): every plugin, the App Server's host plugins included, becomes one process row that registers its Session and branch parts; library hosts and tests add plugins to the Agent builder, and the registry belongs to a placement ([#402](https://github.com/arcships/dim/issues/402)). R6: contributions register with their owner, in every language, and the registration ends with the caller ([#382](https://github.com/arcships/dim/issues/382)). Check every plugin's parts that share a scope and merge those with one owner and no reason to stay apart, such as reload isolation; a merge is its own commit, separate from the migration. Delete the `Capability` trait, the scope types, the `In*`/`*Gate` wrappers, the `*Service` markers, `session_plugin`/`timeline_plugin`, `session_part`/`branch_part`, the Session and branch rows and the name pre-registration. Regenerate the plugin pages, grouped by plugin | `xtask check` passes; no layer type remains in code or documentation; the configuration holds no Session or branch row; a plugin loaded while Sessions are open has its parts mounted into each, and its unload removes them; every merge lands in its own commit; existing multi-Session and multi-branch flow tests, ordering, cancellation and facade restrictions unchanged |
| P4 | Cross-language parity: R2, R3 and R5. Their first form was delivered in #376; R5's second form, a foreign plugin as one row that registers its parts in code as rutis plugin definitions, is delivered for TypeScript and Python ([#403](https://github.com/arcships/dim/issues/403)); export Dim's cross-language services with `#[rutis::service]`; the authoring guide says which names a row registers or isolates inside an instance ([rutis usage §3.5](rutis-usage.md#35-how-each-runtime-resolves-a-name)); foreign runtimes' output reaches the process log instead of being discarded ([ADR 005](../adr/005-observability.md)); the SDK and the authoring guide change with it; the private managed-provider plugin follows | A TypeScript and a Python plugin, each one row, register a Session and a branch part that mount without host inference, consume facts, submit inputs and provide a tool; an unmodified Cordis 4 plugin registered as a branch part gets its own branch's services. The Node-backed flow tests pass |
| P5 | R1 | Every Capability rebuild test passes with the one-registration form; the hand-written form still passes |
| P6 | Guidance and generated documents final: the plugin pages, the authoring guide, single-tree and lifecycle describe the implemented contract | `xtask check`; no document requires an API that does not exist |

Issue split, replacing #370–#376 when this design is merged:

| Existing | Becomes |
| --- | --- |
| #370 (design) | Closed by this design |
| #371 (`Service<T>` and raw mount APIs) | P2, without `Service<T>`: catalog registration and name dependencies |
| #372, #373, #374 (migrations) | P3, the same three batches plus process and simple parts |
| #375 (delete the old API) | The end of P3 |
| #376 (raw rows for other languages) | P4 (R2, R3, R5) |
| #362 (`SkillsCatalog` watch) | Closed: on the flat bus the event reaches every Session row |
| new | P1 (0.16 upgrade), P5 (R1), R4, R6 |

## 11. Rule check

This design changes the plugin infrastructure's contract, not a business plugin.

| Rule | Answer |
| --- | --- |
| Principles §1 (plugins and Capabilities are bounded) | Capability narrows to a state machine on a branch Timeline (§6). No wiring module is added; the App Server's relays are removed (0.16 upgrade U5) and its group inference too (rutis usage R5) |
| §4 rule 1 (commands) | Unchanged |
| §4 rule 2 (handover) | Unchanged |
| §4 rule 3 (`before`) | Unchanged; now stated as in process only (§5.1) |
| §4 rule 4 (core has no business meaning) | The catalog, R1 and R3 are generic; no plugin-specific handling is added |
| §4 rule 5 (batches) | Unchanged |
| §4 rule 6 (context) | Unchanged |
| §4 rule 7 (public interaction only) | Unchanged |
| §4 rule 8 (infrastructure unchanged by default) | R1–R7 each come from the mechanism's own responsibility and close inside it; rutis is used as it is |
| §4 rule 9 (only needed facts) | No fact is added |
| ADR 002 D1–D5 | D1–D4 unchanged; D5 completed: one named catalog, registration declared by the plugin (R6), Rust as a binding |
