# 006: The plugin contract is concepts and conventions, not types

Decision status: Accepted by the repository owner (2026-10-24); D4 amended the same day to say how
each runtime resolves a name, where a Rust key comes from, which names a part may provide inside an
instance and how prefixed providers are found. D3 replaced the same day: a plugin is one row, and
each Session and branch mounts the parts that plugins register for it. D3 and D4 amended on
2026-10-25: where plugins run, contributions registering with their owner, and foreign plugins
waiting for rutis#642. Context and D4 amended on 2026-10-26: the Context is marked as the state
before the migration and says which of its mechanisms the migration removed, and D4 states that
removal as done; the notes on the layer-typed API and on `sessionLifecycle` state the current fact.
D3 amended the same day: a foreign plugin registers its parts as rutis plugin definitions, which
rutis 0.17 delivered for rutis#642. D3 amended on 2026-10-26 by the plugin management design: an
installed plugin directory is one group row with at most two children, its code row and its content
row. Implementation: P1, P2, P3, R6 and P4 done; R5's second form with #403. The phases are in the
[plugin contract design](../design/plugin-contract.md#10-migration).
[中文](../zh/adr/006-plugin-contract.md)

Relation to other decisions:

- Extends [ADR 002](002-plugin-scope.md). Its D1–D4 stand unchanged. This ADR finishes D5:
  Dim's own runtime services become named contract entries, and Rust becomes one binding of
  the contract instead of its definition.
- Keeps [ADR 001](001-harness-foundation.md#events-and-ownership-issue-2) (envelope, reserved
  domains, descriptive event declarations) and [ADR 003](003-approval-scope.md) (trusted
  in-process plugins are not isolated by declarations).
- Is documented in two designs, kept apart: [How Dim uses rutis](../design/rutis-usage.md) (D2–D4)
  and the [plugin contract](../design/plugin-contract.md) (D5–D8).
- Supersedes the event-scope model of the [rutis 0.15 upgrade design](../design/rutis-0.15-upgrade.md)
  (§4, D2–D4). The [rutis 0.16 upgrade design](../design/rutis-0.16-upgrade.md) replaces it.
- Supersedes the layer-typed authoring API as the target: the `Capability` trait and its `keys`,
  `SessionScope`/`BranchScope`, `InSession`/`InBranch`/`SessionGate`/`BranchGate`,
  `SessionService`/`BranchService`, and `AgentBuilder::session_plugin`/`timeline_plugin`.
  The migration removed it; D4 lists what was removed.
- Replaces the unmerged instance-mounting draft (#377) and the decisions of #370.

## Context

This section records why the decision was made, so it describes `main` at `dd87f6af`, before the
migration. The decision status above records what has been implemented since; [How Dim uses
rutis](../design/rutis-usage.md) says what holds today.

Dim's architecture rests on a small theory. The [time bus](../architecture/rust-agent/time-bus.md)
fixes order by sequence, gives each state one owner, routes changes to an owner as commands, makes
handover explicit, limits `before` to transforming a draft, rebuilds state from history and never
replays side effects. ADR 002 D1 adds one placement rule: a part lives where the owner of its state
lives. None of this depends on a programming language.

The plugin API and the guidance around it express this theory as Rust types:

| Concept | Current expression | Extent on `main` (dd87f6af) |
| --- | --- | --- |
| Where a part lives | `session_plugin(\|scope: &SessionScope\| ..)`, `timeline_plugin(\|scope: &BranchScope\| ..)`, `Capability::keys` | 199 `Capability` implementations |
| Which instance a dependency comes from | `InSession<T>`, `InBranch<T>`, `SessionGate<T>`, `BranchGate<T>`, `SessionService`/`BranchService` markers | 221 uses |
| Isolation of event flows | `EventKey::of().instance(id)` | 9 sites, an API rutis 0.13 removed |
| Rules for authors | AGENTS.md rule 1, [principles §1](../architecture/rust-agent/principles.md), [single-tree](../architecture/rust-agent/single-tree.md#实例键与可见性), the plugin page README | 27 English documents name these types |

Four consequences follow.

1. **Non-Rust parts need per-layer bindings that re-derive what the Rust types encode.**
   - The App Server infers a TypeScript row's group from whether its `inject` names a branch
     service (`crates/dim-app-server/src/session_typescript.rs:373`), and hard-codes which service
     prefixes each group may provide (`:400`, `:457-492`).
   - A service's group is declared three times with no cross-check: the `SessionService`/`BranchService`
     markers, `build.rs` `service_in(.., "branch")`, and `BRANCH_SERVICES`.
   - One concept has two identities. A native tool contribution is
     `TypeKey::keyed_dynamic::<ToolContribution>`, a foreign one is `host_key_in("toolContribution:..")`.
     ToolRegistry reads both, and it detects a retiring foreign generation through a sidecar
     cancellation token that it finds by the same string (`tool_registry.rs:486-509`).
   - The host pre-registers provider names (`session_tool`, `session_methods`, `session_prompt_source`,
     `session_tool_source`, `host_tools`), and translates a foreign row's `provides` into those calls.
     ADR 002 D5 says registration is declared by the plugin, not by host-side `AgentBuilder` calls.
   - Session and Settings events reach foreign rows only through a relay on `Process::emit`
     (`:166`, `:251`, `:515`), an API rutis 0.13 removed.
   - TypeScript Session rows have no activation service, although Rust Session parts have one
     ([authoring guide](../typescript-plugin-authoring.md#plugin-shape-and-lifetime)).
2. **Every consumer re-implements the same infrastructure protocols, in every language.** Each
   ordered consumer subscribes, fixes the committed cutoff, folds local history and skips covered
   events: `crates/dim-agent-capabilities/src/wait.rs:361-380`,
   `runtime/plugins/goal/branch.ts:120-128` and the authoring guide's example all do it. A
   TypeScript part translates native error variants into JSON-RPC `appError` codes itself
   (`runtime/plugins/goal/session.ts:15`), which is a boundary conversion done inside a plugin.
3. **Guidance mixes four kinds of statement:** the theory, conventions, the current Rust spelling,
   and delivery status or history. A contributor cannot tell which sentence is a rule and which
   describes today's code. Design "Rule check" sections therefore restate the list instead of
   answering it.
4. **rutis changed underneath, from 0.13 to 0.16:**
   - One flat event bus per root. Every listener of a name receives the event, wherever it was
     registered. A listener cannot see the emitter's instance, and event payloads must be serde.
   - Instance services by name. A catalog name belongs to one instanced group and resolves, for each
     row copy, to the nearest enclosing instance of that group. A row placed outside that group
     fails at composition, whether it is Rust or any other runtime (`ServiceCatalog::register_instance`,
     `register_shared_instance`, `key_in`, `Build::instance`).
   - `#[rutis::service]` serves a Rust service by name in the instance where it is provided.
   - rutis's stated design: "Dependency declarations contain only service names". The model is
     implemented once, in the Rust core, and other languages are leaves with a small SDK. rutis's
     own agent plugin uses plain plugins, domain service traits and named events, with no plugin
     trait of its own.

The 0.15 upgrade design (#361) planned around scoped delivery, which rutis 0.15.1 withdrew. The
#377 draft moved toward the rutis model but kept two things this ADR rejects. It kept a Rust
`Service<T>` identity next to names. It also proposed a loader-enforced Capability role, an
access check that [ADR 003](003-approval-scope.md) says trusted in-process plugins do not have.

The mechanisms item 1 lists have since been removed: a row declares its own group instead of the App
Server inferring it, each name and its group are declared once
([How Dim uses rutis §3.4](../design/rutis-usage.md#34-the-catalog)), a tool contribution has one
identity registered with its owner, contributions register from the part that offers them so the
host pre-registers none and translates no `provides`, and rows of other languages get publication
and closing from named services such as `sessionLifecycle` and read the bus directly instead of a
relay. What remains is R5's second form: a foreign feature is still one configured row per part,
each declaring its group, until #403, which removed it.

## Decisions

### D1. Separate layers, one authority each

| Layer | Content | Authority | Changed by |
| --- | --- | --- | --- |
| Dim architecture: theory | Sequence, ownership, commands, handover, `before`, rebuild, side effects, placement by owner | [time bus](../architecture/rust-agent/time-bus.md), [principles](../architecture/rust-agent/principles.md), [ADR 002](002-plugin-scope.md) | A decision record |
| Dim architecture: plugin contract | What the instance tree means, placement, Dim's services and public bus events, Timeline facts and their consumption, how bus events are consumed, Capability, publication and closing | [plugin contract](../design/plugin-contract.md) | A design change under this ADR |
| Using rutis | Parts as loader rows, services by name and group, the catalog mechanism, crossing languages, the rutis lifecycle and event bus | [How Dim uses rutis](../design/rutis-usage.md) | A design change under this ADR |
| Binding | How one language writes both: Rust traits and macros, TypeScript/Python `definePlugin`, generated declarations | The appendices of How Dim uses rutis, SDK sources, generated files | An implementation change |

The two middle layers are kept apart. D2–D4 decide how Dim uses rutis; D5–D8 decide Dim's
architecture on top of it. A Dim rule is written against what rutis guarantees, never against a rutis
API, and a rule about using rutis decides no business question.

A binding adds no semantics. When a binding needs a rule that no layer states, that layer changes
first. Rust is a binding: it is the most complete one, not the definition. Rules for contributors
(AGENTS.md, principles, templates) are stated at their layer and link to a binding for spelling.

### D2. rutis owns the plugin machinery; Dim defines its domain on top of it

rutis provides the following, and Dim uses it directly:

- plugin lifecycle (apply, effects, unload, cancellation, deadlines);
- dependency gating, Pending and reload propagation;
- services by type and by name, instanced groups and per-instance name resolution;
- loader rows and configuration;
- language runtimes, cross-language calls, the event bus and diagnostics.

Dim defines:

- the instance tree, when its instances open and close (the `agent`, `session` and `branch`
  groups), and the part registry through which each instance mounts the parts plugins register
  for it (D3);
- the Timeline mechanism (append, sequence, batch, publication, `before`, Dispatcher);
- the catalog of its named services and public bus events;
- event contracts and reserved domains;
- the part conventions (D6), the assembly profiles and the audit (D9).

Dim adds none of the following: a plugin trait, author-facing scope types, a resolver, a manifest,
a role field, a second service directory, a second bus, or per-language relays and adapters.
If a need seems to require one, check rutis first. A real gap is solved inside Dim's own
mechanisms, following [principles §4 rule 8](../architecture/rust-agent/principles.md); rutis stays
an external dependency.

### D3. A plugin is one row; each instance mounts the parts registered for it

- **A plugin is one process row.** Every plugin is one rutis plugin mounted as one loader row in the
  process, whether native or foreign, built in or optional. Its configuration, enablement and reload
  are that row's. What the plugin owns for the process, it holds itself.
- **Parts are registered, not configured.** For the `session` and `branch` scopes, a plugin
  registers part generators with Dim's part registry while it runs. A registration belongs to the
  plugin's generation: it is withdrawn when the plugin unloads, and the next generation registers
  again.
- **The instance mounts.** When a Session or branch opens, it calls every generator registered for
  its scope and mounts each result as its own child. A mounted part lives inside that instance: its
  names resolve there (D4), and it closes with the instance. A plugin never reaches into an
  instance, and an instance mounts parts only into itself.
- **Registrations follow the open instances.** A generator registered while instances are open is
  mounted into each of them. A withdrawn one is unmounted from each, and the withdrawal finishes once
  those parts have unloaded.
- **What a plugin registers.** In Rust, a generator: it returns a plugin built for the instance from
  its keys (D4), or nothing for an instance. It sees the instance's identity and configuration, and a
  Session whose configuration turns a feature off gets no part of it at all. A plugin in another
  runtime registers a rutis plugin definition, made with its runtime's `definePlugin`; each instance
  mounts one copy of it, and the copy runs in the plugin's runtime. A definition cannot return
  nothing, so a copy whose instance's configuration turns the feature off does no work. Choosing per
  instance goes through business configuration such as `SessionConfig`, never through rows.
- **Publication.** The host's assembly profile decides which parts a Session or branch waits for
  before publication ([single-tree](../architecture/rust-agent/single-tree.md#结构)). A required part
  must be Active, not merely loaded. Every other part registered when the instance opens settles
  before publication, as the instance's optional rows do, so what it registers (tools, methods) is
  there once the instance is published; one that is Pending or Failed affects only its own features.
  A part mounted after publication does not publish the instance again. A plugin does not declare
  that it is required.
- **An installed plugin directory.** A plugin installed from a directory is one group row whose
  children are at most its code row and its content row, which registers the directory's content
  with each owner. Enablement, reload and removal are the group's, so the user still sees one
  plugin ([PluginManager §3.2](../design/plugins/PluginManager.md#32-a-plugins-rows)).
- **Inside a part.** A part may still create children in its own subtree, as McpSession does for each
  server and Skills for each content. They are the part's business.
- **Where plugins run.** The App Server places plugins as process rows of its loader. A library
  host or a test adds them to the Agent builder, and an Agent that places itself mounts them in its
  own loader. The registry belongs to a placement, so an Agent that places itself mounts only the
  parts its own plugins register.

Placement follows ADR 002 D1 unchanged: a plugin registers a part for each scope where it owns
state. Nothing else encodes placement: no plugin trait, no factory signature, no inference from
injected names, and no Session or branch rows in configuration. The `agent` group holds the host's
facade, not feature parts. The details are
[How Dim uses rutis §2](../design/rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts).

### D4. A service is a name in one group, and the catalog is the contract

- **Identity.** Every service that one part offers another has a camelCase name and exactly one
  group: process, `session` or `branch`. A name never falls back to another group (rutis rule). A
  set of contributions to one owner, such as the tools of a Session, is not a set of names: each
  contribution registers with the owner (below).
- **One catalog.** Each Dim name is declared once. A service that crosses languages declares its
  name on its Rust type with `#[rutis::service]`; a Rust-only service, or a name a foreign part
  provides, is registered in the host's loader catalog. The catalog shares only registered and
  marked names (`share_registered_only`), so exactly the catalog crosses languages. `xtask`
  publishes it as a generated table from the real assembly: name, group, provider, methods (sync or
  async, serde request and reply shapes), errors, and whether the service crosses languages or is
  Rust-only.
- **Dependencies by name, in every language.** A part names the services it injects and provides;
  nothing in it chooses which copy it gets. For each copy, rutis resolves a name to the nearest
  enclosing instance of the name's group, with no fallback, and rejects a part that names a group
  its instance is not inside. This is rutis's model for per-instance services: the per-instance key
  belongs to the framework, not to the part. Each runtime carries the rule out in its own form:
  - In Node and the other foreign runtimes, rutis isolates each such name under the instance's label
    (Cordis `isolate` in Node). The part reads the plain name and gets its own instance's copy; an
    unmodified plugin for rutis's Cordis 4 works this way.
  - In Rust, the binding takes each key from the catalog for the name and the copy's instances, so a
    provider is at one key whatever language reads it. A name registered with a Rust type has that
    type's key, qualified by the group's instance; the binding names such a type as
    `#[rutis::service]` does, so one Rust type is one name. A name that crosses languages is at its
    dispatcher's key (`host_key`, qualified the same way), where a Rust part reads a provider of any
    language. An exported Rust service is also at its native key, which its export serves from and
    which Rust parts of its own type read.
- **Isolation is the framework's.** Placement and the catalog decide which copy a part sees. A part
  does not isolate the services it shares. A row's `isolate` remains for names private to that part
  or to a group of parts, as in Cordis; the catalog does not share them.
- **A name meant for one instance is registered.** rutis publishes a service that a row announces
  without declaring it, as a Cordis plugin does, under its plain name for the whole process. Inside a
  Session or branch that makes it one process-wide service: other instances see it, and a second copy
  that provides it fails with rutis's duplicate-provider error. So a name a part provides for its own
  instance is in the catalog with its group, and a name private to the part is isolated by its row.
  Dim adds no check of its own.
- **Rust-only services stay in the catalog.** A service whose methods cannot cross languages (a
  callback into the commit lock, a handle, a channel) is still named and grouped, and the catalog
  marks it Rust-only. To make it cross languages, give it serde-shaped methods; there is no
  hand-written adapter layer.
- **Errors are data**, `{code, message}` with documented codes (ADR 002 D5). The wire boundary
  (Rpc) maps a service error to a wire error, not the part that forwards it.

- **Contributions register with their owner.** Tools, tool sources, prompt sources, Session method
  namespaces and management method namespaces each have one owner that keeps their table:
  ToolRegistry, PromptRegistry or Rpc. A part that contributes one calls the owner's `register` from
  its own code, reaching the owner by name like any dependency. The registration belongs to the
  caller and is withdrawn when the caller unloads. The host does not pre-register contributions, the
  owner does not search for them, and a contribution has one identity in every language: its
  registration.
  - In Rust the caller passes its ctx. From another language the bridge ties the registration to the
    calling row, as `sessionLifecycle.hold` does, and the handler is the caller's own function,
    called back through the bridge.
  - A contribution's generation ends when its registration is withdrawn; no side channel marks a
    retiring one.
  - The owner reports a configured name that nothing has registered when it is used, as it reports
    any missing contribution; Session creation does not refuse it.

The implementation removed: the `Capability` trait and its keys, `SessionScope`/`BranchScope`,
`InSession`/`InBranch`/`SessionGate`/`BranchGate`, `SessionService`/`BranchService`,
`session_plugin`/`timeline_plugin`, the host-side name pre-registration, the `toolContribution:*`,
`sessionMethods:*` and `managementMethods:*` names with their native/foreign split, the Session and
branch rows, those of other languages included (#403), the App Server's inference of a row's group,
and the per-prefix placement checks that the catalog's groups already express. `PublicService`
stays. It limits which branch services the Session and branch facades expose to ordinary callers,
which is a facade rule, not a plugin rule.

The details are [How Dim uses rutis §3](../design/rutis-usage.md#3-services-by-name); the names Dim
defines are listed in the [plugin contract §4](../design/plugin-contract.md#4-dims-services).

### D5. Timeline facts and bus events are different things

| | Timeline fact | Bus event |
| --- | --- | --- |
| What it is | A durable, sequenced fact of one branch, in the `AgentEvent` envelope | An ephemeral rutis event on the root's flat bus |
| Written by | The branch's `timeline` service; the producer guarantees the payload | Any part, through rutis `emit`/`parallel` |
| Delivery | The branch's `dispatcher`: in sequence order, every fact to every part registered in the generation | The listeners registered when it is emitted; not stored or replayed; no order across names; `emit` does not wait |
| How a consumer recovers | It rebuilds from the Timeline | As [time bus §3](../architecture/rust-agent/time-bus.md) asks of every state machine: from the Timeline, or by re-reading the owner's state. A missed bus event can delay a decision, not change it |
| Identity | Domain and name; core domains are reserved | A public name in the catalog; serde payload |
| Isolation | Per branch, by construction | By name. A bus event that belongs to one instance carries that instance's identity in its name, built by a constructor the owner exports, or the owner offers a watch or a service method instead |

Instances bound lifecycle and service visibility. They do not bound event delivery. The bus names
that the Dispatcher and Timeline use internally belong to them. Parts consume through `dispatcher`
and register transforms through `timeline.before`, never by those names.

### D6. Capability is a convention: a state machine on a branch Timeline

A Capability is a part that keeps state folded from a branch's facts in sequence order (it consumes
through `dispatcher`), or writes facts of its own domain (through `timeline`). It is defined once, in
[principles §1](../architecture/rust-agent/principles.md). Other parts get no name of their own; they
are parts, and the existing rules apply to them as to every part. Timeline, Dispatcher, TimelineTree,
the scope roots and the facades are mechanisms with their own contracts, not parts in this sense.

This changes the meaning of the word. Before, principles §1 called every Session or branch part a
Capability. Now a Capability is a state machine on a branch Timeline. A Session part that only
provides tools or methods, or only writes inputs, is not a Capability. The capability reload
protocol ([single-tree](../architecture/rust-agent/single-tree.md#能力重载协议)) applies to
Capabilities.

Being a Capability is not declared and not enforced: there is no role field, no trait and no loader
check. Trusted in-process parts are not isolated by declarations (ADR 003, AGENTS rule 11), and on a
flat bus a loader check could cover injection but not listening. Such a check would look like a
boundary without being one. Whether a part is a Capability shows in the names it injects; the audit
(D9) derives it from the real assembly.

### D7. What the mechanisms guarantee, and what they do not

The mechanisms guarantee:

| Concern | Guarantee | Owner |
| --- | --- | --- |
| Visibility | A group's name resolves only inside that group's instances; a misplaced row fails at composition | rutis |
| Exposure | Only catalog names cross languages | rutis catalog, Dim registration |
| Timeline integrity | Envelope check, reserved core domains decoded by their typed contracts, atomic batches, sequence, publication after commit, refusal after close | Timeline (ADR 001) |
| Order | Per-branch delivery in sequence order; the rebuild cutoff semantics | Dispatcher |
| Lifecycle | apply, effects, unload, cancellation and deadlines; publication only after the profile's required parts are Active | rutis, Session and branch assembly |
| `before` | In process, synchronous, inside the commit lock. No foreign runtime registers one ([ADR 001](001-harness-foundation.md#trust-and-cross-language-calls): no cross-process code inside commit critical sections) | Timeline |

Everything else, that is the rules of the time bus, principles §4 and ADR 002, is convention.
Review, the audit (D9) and flow tests check it.

### D8. Protocols every consumer repeats belong to the mechanism's owner

- **Ordered consumption with rebuild becomes one Dispatcher operation.** One registration delivers
  local history through a fixed cutoff, then live events after it, in every language. The
  hand-written form stays valid.
- **The Session and branch scopes each have a lifecycle service, in every language:**
  `sessionLifecycle` (`activated`, `closing`, `hold`) in `session` and `branchLifecycle`
  (`activated`, `closing`) in `branch`. The process scope has none; process parts follow the host's
  start and shutdown through the rutis lifecycle. Both services are in the catalog in their groups,
  so every language reads them (R2, delivered in phase 4).
- **Rpc maps errors to the wire.** Errors of contributed methods are mapped at Rpc. A part rethrows
  the native error data it received.

These are raised as requirements R1–R3 in the contract's [requirements section](../design/plugin-contract.md#9-requirements-raised-to-mechanism-owners),
in the format of principles §4 rule 8; the Dim runtime and the App Server own them. An SDK may add
sugar over the contract, but no semantics (D1).

### D9. Guidance is layered, and the audit is generated

- Each rule is stated in one place. AGENTS.md and principles §1 state rules without Rust spellings,
  which live in the Rust binding. The contract links to rules instead of restating them.
- A design document's "Rule check" says which parts are Capabilities and answers the existing
  list, as before: principles §4 rules 1–9, §1 and ADR 002 D1–D5. The pull request template is
  unchanged.
- Delivery status and history live in ADR delivery tables and design documents, not in rules.
- `xtask` derives from the real assembly which parts are Capabilities (they inject `dispatcher` or
  `timeline`) and writes it into the generated documents, together with the catalog table.

### D10. Order of work

1. The rutis 0.16 upgrade, under [its own design](../design/rutis-0.16-upgrade.md). The relays go
   with it: rows listen to the public bus events directly.
2. The catalog and the Rust name binding.
3. The part registry and the instances' mounting (D3). Every plugin becomes one row that registers
   its Session and branch parts, every part takes name dependencies, contributions register with
   their owner, and the layer types and the host-side pre-registration are deleted. The old types exist only
   until this step has moved every part.
4. Cross-language parity: the Session and branch scopes get their lifecycle services in every
   language and Rust services are exported through the catalog (done in #376); foreign plugins
   register their parts as plugin definitions (#403), which rutis 0.17 provides for rutis#642.
5. The mechanism requirements of D8 (R2 and R3 done in #376, R1 in #380).
6. Guidance and generated documents follow each step.

Dim is unreleased, so no step keeps a compatibility path: no aliases, shims or transition shapes for
anyone outside the repository. The TypeScript SDK, the repository's plugins, the private
managed-provider plugin and the documents change with the step that needs it. This design changes no
persisted or wire format.

## Consequences

- A service has one identity across languages, so Rust and TypeScript parts in one instance use each
  other's services by name.
- Fewer Dim-specific types. Plugin pages and design documents stop restating Rust spellings.
- The word Capability narrows. Documents that use it for every Session part are revised when they
  are next touched; principles §1 is revised now.
- Being a Capability is not enforced, so a faulty trusted part can still write another owner's domain or listen
  anywhere. Review, the audit and flow tests guard against that, as they do today.
- The private managed-provider plugin follows each SDK change.
- Cordis compatibility means rutis's Cordis 4 (`@deepseek-ai/cordis`). Plugins for Cordis 3 (the
  Koishi generation) are not a target: their optional injects and intercept options do not gate.
- A feature is one plugin in every language: one row, one configuration, one reload. Its process
  state and the parts it registers live in one module or package.
- The mechanics of D3 are Cordis's: a registration owned by its caller, as `ctx.on` is; a mounted
  part owned by the instance that mounted it; isolation by the instance's labels. How they are put
  together, an instance pulling the generators registered for its scope, has no precedent in Cordis
  or Koishi, where one plugin with filtered contexts is the norm.
- A Rust feature became one row with #402, and a TypeScript or Python feature with #403. A foreign part cannot skip an instance as a Rust
  generator can; its copy is mounted and does no work there.
- The 33 plugin pages under `docs/architecture/rust-agent/plugins/` are regrouped by plugin in step 3.

## Alternatives considered

| Alternative | Why not |
| --- | --- |
| Keep the layer types and add Rust sugar (`Service<T>`, #370/#371) | Adds a second identity beside names; foreign parts still need host-side placement inference |
| A loader-enforced Capability role (#377) | Looks like isolation without being one (flat bus, trusted plugins); a privileged-service list to maintain |
| Keep per-layer cross-language bindings (C4, C5, process) | Each re-derives placement, lifecycle and delivery, and they already diverge (no Session activation in C4) |
| Put the conventions into SDK helpers | Replicates the framework per language, against rutis's design and D1 |
| Scoped event delivery (the 0.15 design) | Withdrawn by rutis 0.15.1 |
| Per-instance services by `isolate` on the group rows (the Cordis idiom) instead of instance keys | rutis builds per-instance services on instance keys: kernel visibility, composition checks, exports to other languages and marked-service claims assume them, and exports cannot see an isolated native. A group's isolation does not reach foreign rows, each isolated key must be listed up front (state cells, named families and runtime-generated names are open sets), and a misplaced row would wait instead of failing. Node already turns instance names into Cordis isolation |
| Session and branch parts as configuration rows (the first form of D3) | Splits one feature into rows that its author, and every foreign configuration, places one by one. The plugin is only a convention, and its process part is registered elsewhere |
| Only process plugins, each keeping its per-Session and per-branch state inside itself | Gives up what an instance gives its parts: the instance's lifecycle and its own services, readiness before publication, and failure that stays local |
| A plugin mounts its parts into instances itself, on a Session-created hook | The instance would no longer own its subtree: readiness and failure would not be its decision, and plugins would hold instance contexts |
| Templates registered at run time in the loader's instanced groups | Needs a loader change and gives nothing more: an instance mounts its parts with rutis's `ctx.plugin` |
