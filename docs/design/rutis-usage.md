# How Dim uses rutis

Status: Accepted under [ADR 006](../adr/006-plugin-contract.md) (D2–D4). Each section states the
contract.
[中文](../zh/design/rutis-usage.md)

rutis owns the plugin machinery. This document states how Dim uses it: how a part is mounted, how it
names and finds services, how a service crosses languages, and what the rutis lifecycle and event bus
give. It holds for every part, in any language, and its appendices are the Rust and TypeScript
bindings.

It does not state Dim's architecture. What an instance means, Timeline facts, ownership, Capability,
publication and closing are in the [plugin contract](plugin-contract.md), on top of the
[time bus](../architecture/rust-agent/time-bus.md) and the [design rules](../architecture/rust-agent/principles.md).
A rule here decides no business question, and a rule there does not depend on a rutis API.

## 1. What Dim takes from rutis

Dim uses these directly, without wrapping them ([ADR 006 D2](../adr/006-plugin-contract.md#d2-rutis-owns-the-plugin-machinery-dim-defines-its-domain-on-top-of-it)):

- plugin lifecycle: apply, effects, unload, cancellation, deadlines;
- dependency gating, Pending and reload propagation;
- services by type and by name, instanced groups and per-instance name resolution;
- loader rows and configuration;
- language runtimes as the far ends of rutis sessions, cross-language calls, the event bus and
  diagnostics.

A local runtime is a connection row of the host: its plugin starts the runtime's process with
rutis-bridge's public parts and mounts the session on it (appendix A.4). This is the form rutis
documents for a host's own connection, not a wrapper.

Dim adds no plugin trait, author-facing scope type, resolver, manifest, role field, second service
directory, second bus, or per-language relay or adapter.

## 2. Plugins are rows; instances mount their parts

A plugin is one rutis plugin, mounted as exactly one loader row in the process. Its Session and branch
parts are not rows: the plugin registers them, and each instance mounts them
([ADR 006 D3](../adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it)).
Which scope a part belongs in is Dim's decision ([plugin contract §3](plugin-contract.md#3-placement)).

| Row field | Meaning |
| --- | --- |
| `id` | Stable row identity in the configuration |
| `name` | The module: a built-in name; a path or npm name, which runs in the Node runtime; or `<runtime row>:<module>`, such as `py:<module>` for the Python runtime |
| `config` | Structural configuration, validated by the plugin's schema |
| `inject` | The names the plugin depends on. A foreign plugin's own `inject` must equal this list or be a subset of it |
| `isolate`, `disabled` | rutis isolation labels; whether the row is off |

**Registering a part.** The part registry is a process service. A plugin registers a generator for
`session` or `branch` from its `apply`. The registration is an effect of the caller, as Cordis ties
`ctx.on` to its caller: it lasts while the plugin's generation runs, and it is withdrawn when that
generation unloads, however it ends. A generator's dependencies are checked against its scope when an
instance mounts it, so a part that names a group outside its scope fails there.

**Where plugins run.** A host has one loader, and its Agents are built in it
(`AgentBuilder::build(loader, anchor)`). The App Server places plugins as process rows of its loader
and provides the registry to them by the process name `partRegistry`. Tests, examples and `xtask` are
hosts like the App Server: the minimal host (`dim_agent_runtime::testing::MinimalHost`) assembles one
root and its loader the same way, with the plugins added to the Agent builder (`AgentBuilder::plugin`)
as process rows. Each minimal host has a registry of its own, so two Agents built from one builder in
two hosts do not mount each other's parts.

**Isolation labels.** A label that separates rows is numbered by one process-wide counter, not one per
loader or builder. A per-builder counter restarts at 1, so two Agents built from one builder isolate
their values under the same label, and generated descriptions of the assembly change between runs.
Numbering in the process prevents this.

**Mounting.** Every Session and branch instance holds two mounter rows, the only rows of its group
besides the scope's own mechanisms.

- When the instance opens, each mounter calls the generators of its scope it owns and mounts each result
  with rutis's `ctx.plugin` as its own child; a generator that returns nothing for this instance mounts
  nothing.
  - The required mounter owns the parts the assembly profile requires. It hands them to the instance's
    creation, which waits for them to be Active as it waits for its required rows; the instance
    publishes only then. It does not wait in its own `apply`, because the loader's instance creation
    waits for every Loading row.
  - The optional mounter owns every other part. It hands the copies it mounts when the instance
    opens to the instance's creation, which lets them settle before publishing, as it does its
    optional rows. A failed or Pending optional part stays local.
- While the instance is open, the mounters follow the registry: they mount a new registration and
  unmount a withdrawn one. A withdrawal finishes when every instance has unloaded its copy. A
  registration that arrives after publication and fails to build is logged and leaves the live
  instance alone; failing the instance instead would tear down a published Session or branch because
  one plugin registered a broken part.
- The instance closes its mounted parts with itself, through rutis's ownership of children.

**What a plugin registers.**

- A Rust generator receives the names of the instance it is mounted in (appendix A) and returns a
  plugin, which may hold what the plugin prepared in its `apply`.
- A plugin in another runtime registers a plugin definition instead of a function (R7): a value made
  with its runtime's `definePlugin`, which may close over what the plugin prepared in its `apply`.
  Each instance mounts one copy of it with rutis's `Definition::at`; the copy runs in the plugin's
  runtime, its `inject` resolved in that instance. When the registering generation unloads, rutis
  ends every copy and then the definition (`Definition::ended`); the part registry drops the
  registration once the definition has ended, so no later instance mounts it. A definition cannot return
  nothing for an instance: a copy whose instance's business configuration turns the feature off does
  no work.
- **What a foreign row still declares.** The loader's catalog is fixed before the runtime starts, and a
  name a part provides inside its Session or branch must be in the catalog with that group, or rutis
  publishes it once for the whole process and a second instance's copy fails as a duplicate provider
  (§3.5). A Rust plugin registers such names in the host's builder. A foreign plugin's parts only
  exist once its runtime runs, so its single row lists them, `instanceNames`, each with its group,
  and the App Server registers them before the runtime starts. It is the one thing a foreign row
  declares about its parts: their `inject`, `provides` and everything else travel with the
  definition.

**Every plugin and part:**

- A factory or module top level reads only its configuration and identity. It does no I/O and starts
  no work; resources are acquired in `apply` (§4).
- The row holds structural configuration only. Business configuration lives in its owner's source
  ([ADR 002 D4](../adr/002-plugin-scope.md#d4-structural-configuration-and-business-configuration-are-separate)).
- A part may create children in its own subtree, as McpSession does for each server. They are the
  part's business.

*Today:* the part registry and the mounters are implemented (appendix A), and every Rust part is
name-bound (#391, #392, #396–#398, #400).
- Each feature of the capabilities crates and each App Server Session part is a plugin that registers
  its parts: a `Feature` (a plugin that owns nothing for the process) added with
  `AgentBuilder::feature`, or its own plugin added with `AgentBuilder::plugin`. The install functions,
  which are the profiles, require the parts they install by name (`require_feature`,
  `require_part`). The App Server places the builder's plugins as process rows and provides
  `AgentBuilder::parts()` as `partRegistry` (`AgentBuilder::plugin_rows`); the minimal host does the
  same with a registry of its own. A required part nothing has registered
  yet holds an instance's creation as a required row does. A feature that owns process state
  registers its parts from the row that holds it: Mcp, Collaboration, Agents, SkillsCatalog and
  WorkspaceTools, which holds the file-write coordinator. ConfiguredModels and ConfiguredModel, and
  PermissionRules and CommandRules, stay two plugins: the profile chooses which part binds the
  Session's `PrepareModelHost` or `CommandRulesHost`, whether or not the process row runs. The
  minimal host starts its plugins when it starts.
- Tools, tool sources and prompt sources register with ToolRegistry and PromptRegistry, and Session
  and management method namespaces with the Session's `sessionMethods` and the process's
  `managementMethods`, which Rpc reads per call (R6, [#382](https://github.com/arcships/dim/issues/382)).
- `partRegistry` crosses languages: a TypeScript or Python plugin is one process row and registers its
  parts with `registerDefinition` (appendix B, [#403](https://github.com/arcships/dim/issues/403));
  the App Server rejects a row that declares a group.

## 3. Services by name

### 3.1 Identity

- A service is a camelCase name in exactly one group.
- A set of contributions to one owner (tools, tool sources, prompt sources, Session and management
  method namespaces) is not a set of names. Each contribution registers with its owner, which keeps
  the table (R6).
- A name resolves, for each copy of a row, to the nearest enclosing instance of its group. It never
  falls back to another group. A row that names a group it is not inside fails at composition.
- One provider per name per instance. A second provider of the same name in the same instance fails.
- Parts in every language depend on names. A Rust type key is how the Rust binding stores a name,
  not a second identity.

### 3.2 Shape

| Element | Form |
| --- | --- |
| Methods | A name, `sync` or `async`. Callers in other languages always get a Promise |
| Requests and replies | serde data, described as field tables or JSON shapes |
| Errors | Data, `{code, message}`, with the codes documented by the service. A forwarding part rethrows the error it received, and the wire boundary maps it ([plugin contract](plugin-contract.md#9-requirements-raised-to-mechanism-owners) R3) |
| Cancellation | A protocol action: a cancel callback or promise in the request, or the consumer dropping the call |
| Callbacks | By reference, as rutis allows. A callback must not be called inside a commit lock |
| Streams | Callbacks or events; rutis has no stream type |

### 3.3 Crossing languages

A service crosses languages when every method it offers has serde arguments and results. A method
that cannot cross, such as one taking a borrowed value, a channel, a handle or a synchronous callback
inside the commit lock, stays Rust-only. The service is still named and grouped, and the catalog marks
the method or the whole service Rust-only. To make it cross languages, change its shape. There is no
hand-written adapter layer.

### 3.4 The catalog

Each name is declared once. A service that crosses languages declares its name on its Rust type with
`#[rutis::service]`. A Rust-only service, or a name a foreign part provides, is registered in the
host's loader catalog. `xtask graph` writes the authoritative table from the real assembly, the
[generated catalog](../architecture/rust-agent/generated/catalog.md), and `xtask check` fails when it is
stale (R4). What each name Dim defines is for is in the [plugin contract §4](plugin-contract.md#4-dims-services).

*Today:* every service that crosses languages is marked `#[rutis::service]`, the Session and branch
ones included; rutis 0.17 removed the build script's service list (`Exports::service_in`), which
was the other form. A marked type provided in an instance *claims* its name there, and every new
claim runs a loader-wide reconcile, which also rebuilds the copies whose build was rejected: a
required row that failed in a Session's assembly can be rebuilt during that Session's creation, and
each Session or branch creation reconciles the whole loader. The App Server shares those names in
their groups (`register_shared_instance`), and a test checks that each is shared in the group the
Rust binding registers it in. `xtask types` writes their TypeScript declarations to
`npm/branch-services.d.ts`, and `xtask check` fails when that file is stale.

### 3.5 How each runtime resolves a name

The rule of §3.1 is the framework's ([ADR 006 D4](../adr/006-plugin-contract.md#d4-a-service-is-a-name-in-one-group-and-the-catalog-is-the-contract)):
the loader resolves each name for each copy, and each runtime carries the result out. Part code
writes names only.

| Runtime | The part writes | What resolves the copy |
| --- | --- | --- |
| Node (TypeScript, Cordis 4 plugins) | `inject: [name]`, `ctx.use(name)` or `ctx.<name>`, `ctx.provide(name, value)` | The loader resolves the name (`key_in`) and isolates it in the Node process under the instance's label (Cordis `isolate`) |
| Python and the other runtimes | `inject`, `use`, `provide` | The same, through the runtime's isolate table |
| Rust | `injects` lists the name's key and `apply` reads it with `require_at`; it provides `T` at the same key | The binding takes the key from the catalog for the name and the copy's instances (appendix A) |

- **Private names.** A part keeps a service to itself, or to a group of parts, with its row's
  `isolate`, a runtime row as well as a Rust one. The catalog does not share such a name. A
  Session or branch part of another language is a definition copy and has no row of its own, so it
  carries no `isolate`: its names are its instance's.
- **Runtime rows.** A TypeScript or Python plugin's row is resolved through its runtime's session
  (`session-node` or `py`, appendix A.4). While that session is not there, the row waits for it, and
  when the session ends its rows wait again. Dim writes no `isolate` on a runtime row.
- **Sessions export.** Every rutis session exports to its far end each process-wide shared name that
  a non-platform plugin provides, and announces every listener name. A language runtime and a UI
  leaf reach these names alike; they are trusted local code (AGENTS rule 11).
- **Rust keys.** A name registered with a Rust type is at that type's key; a name that crosses
  languages is at its dispatcher's key (`host_key`). Both are qualified by the group's instance. A Rust
  part reads a provider of any language at the dispatcher's key. An exported Rust service is at both.
- **Provided names.** rutis publishes a service a row announces without declaring it under its plain
  name for the whole process. Inside a Session or branch, a name meant for that instance is registered
  with its group, so each instance has its own, and a name private to the part is isolated by its row.
  Otherwise other instances see the service, and a second copy that provides it fails with rutis's
  duplicate-provider error.
- **Required and optional.** A required dependency is injected and gates the part. An optional one is
  read when used, without injecting it: Cordis `ctx.get(name)`, rutis `ctx.get_at(&key)`. A dependency
  the configuration chooses is included in the copy's names or not.
- **Cordis.** An unmodified plugin for rutis's Cordis 4 (`@deepseek-ai/cordis`) runs as a Node row and
  gets the same resolution. Cordis 3 plugins are not a target.

## 4. Lifecycle rutis provides

| Phase | The part may | rutis does |
| --- | --- | --- |
| Build | Read its configuration and identity | Constructs the plugin; no I/O |
| `apply` | Acquire resources, provide services, register subscriptions, register cleanup | Resolves dependencies by name, gates on them, runs `apply` once per generation |
| Unload | — | Runs cleanup in reverse order, takes the part's listeners off the bus, cancels its running calls and waits up to the rutis deadline |

A generation is one load of a part, from `apply` to unload. When a dependency is replaced, rutis
unloads and reloads its dependents along their declared dependencies. Dim adds its publication and
closing phases on top ([plugin contract §7](plugin-contract.md#7-lifecycle-of-a-part)).

## 5. The event bus

Since rutis 0.15.1 there is one flat bus per root.

- **Delivery.** Every listener of a name receives every event of that name, wherever it was registered.
  A listener cannot see the emitter's instance. An event reaches only the listeners registered when it
  is emitted; it is not stored or replayed. Events of different names have no common order. `emit`
  does not wait; `parallel` waits and aggregates failures.
- **Order.** The events of one name start their listeners in emit order: an event's listeners start
  once every listener of the previous one has started, and nothing waits for a listener that awaits.
  A listener that needs order does its ordered work before its first await, or consumes facts through
  `dispatcher` ([plugin contract §5.2](plugin-contract.md#52-consuming-facts)).
- **Payloads are serde.** No handle, channel or lock goes into an event.
- **The name is the contract.** An event that belongs to one instance carries that instance's identity
  in its name, built by a constructor its owner exports, so callers never concatenate names.
  Alternatively the owner offers a watch or a service method.
- **Reserved prefixes:** `rutis/*` (rutis's framework events: listen only) and `internal/*` (each
  runtime's own).

How Dim's parts consume bus events and which public bus events Dim defines are in the
[plugin contract §5.3](plugin-contract.md#53-bus-events).

*Today:* Dim is on rutis 0.19.3, and this section holds. The bus spans every rutis session: each end
announces its listener names, an event reaches the listeners of its name at either end, and every
emit of a language runtime goes through the host.

## 6. Requirements raised to mechanism owners

These requirements concern how Dim uses rutis. They follow
[principles §4 rule 8](../architecture/rust-agent/principles.md), and their phases are in the
[plugin contract migration](plugin-contract.md#10-migration). R1–R3 are in the
[plugin contract](plugin-contract.md#9-requirements-raised-to-mechanism-owners).

### R4. The catalog is generated and Capabilities are marked automatically

- **Blocked:** §3.4 here and [plugin contract §8](plugin-contract.md#8-guarantees-conventions-and-the-audit). The catalog was spread across `build.rs`, the authoring guide and
  design documents, and nothing marked which parts are Capabilities.
- **Existing mechanisms considered:** `xtask graph` already records injected and provided services
  from the real assembly.
- **Needed:** `xtask graph` writes the catalog table (name, group, provider, crosses languages,
  methods) and marks which parts are Capabilities, from the names they inject (`dispatcher`,
  `timeline`).
- **Effect:** none at runtime.
- **Owner:** `xtask`. Delivered ([#381](https://github.com/arcships/dim/issues/381)): `xtask graph`
  writes the [generated catalog](../architecture/rust-agent/generated/catalog.md) from the App Server
  product profile with one Session, and `xtask check` fails when it is stale.

### R5. Foreign plugins register their parts

- **Blocked:** §2. A foreign feature with Session and branch parts is one configured row per part, each
  carrying its group. That form of R5 (foreign rows declare their group instead of the App Server
  inferring it from `inject`) was delivered in phase 4 ([#376](https://github.com/arcships/dim/issues/376));
  the plugin is still not one row.
- **Existing mechanisms considered:** the runtime resolver already builds a copy of a module for the
  instances it runs in (`Resolved::scoped`), and the catalog rejects misplaced names.
- **Needed:** a foreign plugin is one process row and registers its parts with the part registry from
  its code, as plugin definitions (R7). The Session and branch rows of other languages, and the group
  they declare, are deleted. No package declaration is built: a definition carries its own `inject`
  and `provides`, and R6 removed most of the host's need to know names before the runtime starts.
  What remains, the names its parts provide in their instances, stays in the plugin's row (§2).
- **Effect:** none on the Timeline.
- **Owner:** App Server and the SDKs, with the repository's plugins
  ([#403](https://github.com/arcships/dim/issues/403)). Delivered.

### R6. Contributions register with their owner

- **Before R6:** [ADR 002 D5](../adr/002-plugin-scope.md#d5-contracts-are-language-neutral)
  ("Registration: declared by the plugin … not by host-side `AgentBuilder` calls") was blocked: the
  host pre-registered contribution names (`session_tool`, `session_methods`,
  `session_prompt_source`, `session_tool_source`, `host_tools`) and translated a foreign row's
  `provides` into the same calls. A native tool contribution and a foreign one had different keys,
  so ToolRegistry read both and detected a retiring foreign generation through a sidecar token
  found by the same string (`tool_registry.rs:486-509`); Rpc kept the same split for method
  namespaces. Prompt and tool sources were closed to foreign parts. The `Needed` and `Owner`
  bullets below describe what replaced it.
- **Existing mechanisms considered:**
  - Finding contributions by name in the instance, from `rutis/service` events or a catalog listing.
    Rejected: the owner would learn from framework internals (which rows provide which names) what
    the contributor already knows when it calls.
  - Registration with an owner already exists: the part registry (§2) and `sessionLifecycle.hold`,
    which the bridge ties to the calling row (`caller_row`). A registration made through the
    caller's `ctx.effect` is withdrawn when the caller unloads, and the bridge passes a function of
    another language as a callable reference (`RemoteFunction`).
- **Needed:** each owner exposes `register` for its contributions: ToolRegistry for tools and tool
  sources, PromptRegistry for prompt sources, Rpc for Session and management method namespaces.
  Tools and method namespaces register from any language; prompt and tool sources stay Rust-only
  until a foreign part needs one.
  - A registration belongs to its caller. In Rust the caller passes its ctx; from another language
    the bridge ties the registration to the calling row, and the handler is the caller's function,
    called back through the bridge. It is withdrawn when the caller unloads.
  - A second registration of the same name with one owner fails, as a second provider of a name does.
  - A withdrawn handler is not called: a call through a withdrawn registration is refused as not
    made, and a call in flight while it is withdrawn reports that it may already have taken effect,
    as today, without the token.
  - A Session without a mount list (`SessionConfig.tools`) declares every registered tool, so a
    plugin's tools need no configuration edit ([#364](https://github.com/arcships/dim/issues/364));
    a Session with one declares those it names. Session creation does not refuse a named tool that
    nothing has registered; ToolRegistry reports it when the tools are read.
  - Pre-registration, the `toolContribution:*`, `sessionMethods:*` and `managementMethods:*` names,
    the sidecar token and the App Server's translation of a foreign row's `provides` go. ExternalAgent's
    designed `collaborationMembers:<kind>` registers with Collaboration the same way when it is built.
- **Effect:** none on the Timeline. Tool sampling keeps ToolBinding's existing per-turn rule.
- **Owner:** Dim runtime (ToolRegistry, PromptRegistry) and App Server (Rpc),
  [#382](https://github.com/arcships/dim/issues/382). Implemented: tools, tool sources and prompt
  sources register with ToolRegistry and PromptRegistry, and method namespaces with two method
  tables Rpc reads per call, the Session service `sessionMethods` and the process service
  `managementMethods`. Each table checks a namespace when it registers and keeps the row of a
  withdrawn one, so a call answers from that row's status
  ([methods contract A.1](contracts/session-boundary.md#a1-method-contributions)).

### R7. A part is a plugin definition in every runtime

- **Blocked:** §2. A foreign plugin could hand the host only a module name and configuration, because a
  host loaded a foreign plugin by name only. A part prepared from the plugin's own state had to go
  through a shared module or a service, which Rust parts do not need.
- **Existing mechanisms considered:** module name and configuration cover every current part; it was
  the interim form, and it is removed.
- **Needed:** a running row hands the host a part, with its declarations, and the host mounts copies
  of it in given instances. Each copy runs in the producer's runtime, isolated by the instance's
  labels and gated by its `inject`; it belongs to the host fiber that mounted it; it ends when the
  producer's generation unloads.
- **Effect:** none on the Timeline.
- **Owner:** rutis, [arcships/rutis#642](https://github.com/arcships/rutis/issues/642). Delivered as
  plugin definitions: rutis 0.17.0 for Node, 0.17.1 for Python. A `#[rutis::service]` parameter of
  type `rutis::Definition` receives a definition another language hands over; `Definition::at` builds
  a copy for `ctx.plugin`; a definition belongs to the row that handed it over, and its copies end
  before that row's own cleanups run. A definition is a value, not a generator function, so the
  per-instance choice a Rust generator makes by returning nothing moves into the copy (§2).
  [#405](https://github.com/arcships/dim/issues/405) records the upgrade.

## Appendix A: Rust binding

### A.1 The old forms and their replacements

P3 deleted the old forms ([#375](https://github.com/arcships/dim/issues/375)); this table maps each to its replacement.

| Old form | Replacement |
| --- | --- |
| `impl Capability for P { fn keys(&self) }` | No Dim trait. A part is a rutis `Plugin`, or a `host::DynPlugin` when it depends on names |
| `SessionScope`, `BranchScope` passed to factories | `Names::instance(group)` when a factory needs an instance; identity from `sessionIdentity` or `timelineIdentity` |
| `InSession<T>`, `InBranch<T>` | The key `names.key::<T>()` kept by the part, listed in `injects` and read with `require_at`: a dependency on `T`'s name, keyed by the instance of the name's group in this copy's chain (§3.5) |
| `SessionGate<T>`, `BranchGate<T>` | The name's key listed in `injects` and not read |
| `scope.key::<T>()` when providing | `names.key::<T>()`, the same key a consumer of the name gets |
| `Contributions<T>` | Nothing: a contribution registers with its owner (R6) |
| `When<D>` | An `Option<Key<T>>` the factory fills or not when it builds the copy; listed in `injects` and read only when set |
| `SessionService`, `BranchService` markers | The group registered in the catalog (`names::register`) |
| `AgentBuilder::session_plugin`/`timeline_plugin` | A feature registers its parts with the part registry (`Feature`, added with `AgentBuilder::feature`, or its own plugin with `AgentBuilder::plugin`; a profile uses `require_feature`); `register_part` for an App Server process row |
| `AgentBuilder::session_tool`, `session_methods`, `session_prompt_source`, `session_tool_source`, `host_tools` (removed) | The owner's `register`, called by the contributing part: `toolRegistry`, `promptRegistry`, `sessionMethods`, `managementMethods` (R6) |
| `build.rs` `Exports::service_in(..)` | `#[rutis::service]` on the type, Session and branch services included (§3.4); a type whose source cannot change is wrapped in a marked type of Dim's. `Exports` only writes TypeScript declarations, from `xtask types` |
| `PublicService` | Unchanged (a facade rule) |

### A.2 The binding

A Rust part with no named dependency is a plain rutis `Plugin` (`type Deps` by type). A part that
depends on names is a rutis `host::DynPlugin`, the form rutis gives for dependencies by key: the part
keeps the keys of its copy, which its factory takes from `Names`, lists them in `injects`, and reads
them with `require_at` in `apply` (`get_at` for one read only when present, which is not listed).
Dim adds no trait or adapter of its own. A strict read of a listed key that finds the provider gone
returns its own error, and rutis puts the part back to Pending (A.3). The binding carries out §3.5
for Rust:

- It names a type as `#[rutis::service]` does: `Timeline` is `timeline`, `dyn LoopState`
  `loopState` (`names::name_of`). One Rust type is one name, so a type provided in two groups
  becomes two types, or one group.
- The key comes from the catalog for the name and the copy's instances. `names.key::<T>()` is the
  form for a name registered with the Rust type `T`: `Key::<T>::of().instance(id)` for the instance of
  the name's group, `Key::<T>::of()` at the process. `names.named::<T>(name)` reads any name at the
  catalog's key; for a name that crosses languages `T` is `dyn HostDispatch`, and a provider of any
  language is found there. A provider provides at the key its consumers get.
- A profile part's names are checked against its scope when the assembly is composed: its factory
  asks `Names` for the keys, and a part that names a service of a group its instance is not inside
  fails there, naming the group; so does one that names an unregistered service. A registered part
  gets the same check when an instance mounts it.

The catalog is the builder's (`AgentBuilder::catalog`), and the App Server's loader uses the same one.
`names::register::<T>(catalog, group)` declares a Rust name. The runtime registers its own, each crate
the names of the services it provides, and registering a name again in the same group does nothing.
An exported name is served to other languages through its generated dispatcher, and a test checks that
each export's name and group are the ones the binding derives for its type.

```rust
pub struct WaitBranch {
    dispatcher: Key<Dispatcher>,
    wait: Key<Wait>,
}
impl WaitBranch {
    fn new(names: &Names) -> rutis::Result<Self> {
        // "dispatcher" is a branch name; nothing here says so.
        Ok(Self { dispatcher: names.key()?, wait: names.key()? })
    }
}
impl DynPlugin for WaitBranch {
    fn name(&self) -> &str { "WaitBranch" }
    fn injects(&self) -> Vec<TypeKey> { vec![self.dispatcher.erase()] }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            /* dispatcher.follow(ctx, "Wait", keys, handler).await?, the Rust form of `consume`
               (plugin contract §5.2), then ctx.provide_at(self.wait.clone(), wait) */
            Ok(Effect::Done)
        })
    }
}

// A plugin that owns nothing for the process: it registers a part of every branch, and the profile
// requires it. `names` gives each copy's keys, for its dependencies and for `wait`.
builder.require_feature(
    Feature::new("Wait").branch("WaitBranch", |names| Ok(Box::new(WaitBranch::new(names)?))),
);
// A process row of the App Server's loader.
register_part(&mut builtins, builder.catalog(), |names| Ok(Box::new(Recorder::new(names)?)))?;

// From a process plugin's `apply`: a Session part of every Session that mounts `record` (one
// without a mount list mounts every tool), withdrawn when the plugin unloads. `builder.parts()` is the registry; `builder.require_part(name)` makes
// such a part required.
registry.register(ctx, PartScope::Session, "Recorder", |mount| {
    let mounted = mount.session.tools.as_ref();
    if !mounted.is_none_or(|tools| tools.iter().any(|tool| tool == "record")) {
        return Ok(None);
    }
    Ok(Some(Box::new(Recorder::new(&mount.names)?)))
})?;
```

The required mounter mounts the parts the profile requires (`AgentBuilder::require_part`) and hands
them to the instance's publication with its required rows; a required part nothing has registered
yet holds the creation until it is, within the creation's deadline. The builder's registry
(`AgentBuilder::parts`) serves the Agents the host builds (`AgentBuilder::build`), which provides it
to its process plugins as `partRegistry` (`AgentBuilder::plugin_rows`); the minimal host creates a
registry of its own. A registration is an effect of the caller's `ctx`. The mounters unload their copies themselves before rutis's own cascade, so a copy that
already failed is not reported again when its instance closes, and `SessionLoader` creates instances
from its instance's Session scope row, since the loader places an instance only from a row's
context.

A dependency the configuration enables is an `Option<Key<T>>` the factory fills only when enabled:
`storage: enabled.then(|| names.key::<FileStorage>()).transpose()?`; `injects` lists it when set, and
`apply` reads it only then.

A value the host configures for a process name, such as a scripted `ModelHost`, is
`AgentBuilder::process_value`. The Agent group row (`AgentBuilder::agent_row`) holds its provider and
isolates the name per Agent instance, so neither Agents sharing a host nor the host's own provider of the
name collide with it. The host places that row in its loader, whose catalog is the builder's. `session_value` stays for Session names.

### A.3 Settled questions

Carried from [#6](https://github.com/arcships/dim/issues/6):

- **A lost dependency.** When the provider of a name goes away, rutis returns the dependent to Pending
  and loads it again when the provider is back. The new generation is a restart from the Timeline
  under the capability reload protocol ([plugin contract §7.2](plugin-contract.md#72-generations-reload-and-rebuild)).
  Nothing is added.
- **Optional and configuration-chosen dependencies.** As §3.5: a dependency the configuration chooses
  gates when chosen (an `Option<Key<T>>` the factory fills), and the composition check covers it then;
  one used only if present is read with `get_at` and not listed in `injects`. Its name must still be
  registered.
- **Startup progress.** `startup_timeout` bounds an interval without progress, across loader startup,
  reconcile, required rows becoming Active and opening replay. A row's first activation during the
  opening, advancement of its branch's dispatch sequence, or completion of an opening receipt resets
  that interval. Restarting the same row does not renew it; activity in another Session does not.
  There is no fixed total startup deadline. The
  same rule applies to Session and branch opening, process startup and restart. A branch waiting for
  its parent follows that parent's bounded opening rather than imposing another total deadline.
  A stalled opening fails with its inactive rows and unmet dependencies, and rollback cancels them.
  A composition error or failed required provider fails immediately. Rutis never aborts `apply`:
  rollback waits for it to return after `ctx.cancelled()` resolves.

### A.4 Runtimes and sessions

The App Server's root provides `rutis_bridge::LocalId("dim-daemon")`, the id it greets every session
with. It does not mount `Session::service()`, whose name `sessions` is Dim's `AgentDirectory`.

Each local runtime is a process connection row, `session-node` for Node and `py` for Python. Its
plugin, `RuntimeConnection` (`dim-app-server/src/session_typescript.rs`):

- injects a Rust-only process service carrying its launch command (`nodeRuntime`, `pythonRuntime`),
  so the row starts once the command is provided. The App Server provides it at startup when the
  runtime paths are configured, and RuntimeManager once it has acquired the runtime
  ([RuntimeManager](plugins/RuntimeManager.md)); providing it again restarts the row;
- starts the process with `rutis_bridge::runtime::Launcher` (`--id <row>`), records it in a
  `pipe::Processes` of its own, waits for its greeting (`pipe::greeted`) and mounts
  `Session::new(channel)`. Unloading kills a process that has not ended and waits for it. Standard
  error goes to `tracing` line by line;
- keeps the runtime's state in a value of Dim's own, `RuntimeStatus` (`starting`, `ready`, or `down`
  with a reason), at `RuntimeStatus::key(row)`. The row stays Active when the runtime fails to start
  or its process ends, so the status stays readable; the runtime's rows wait again.

One `rutis_loader::runtime::SessionResolver::npm(catalog, "session-node")`, last in the resolver
chain, resolves the rows of both runtimes: `py:<module>` on the `py` session, every other foreign
name on `session-node`, where the Node runtime resolves npm names. It is attached to the loader
before the loader mounts (`attach`), and follows each runtime row once the loader runs
(`follow_row`). Dim's `SnapshotResolver` wraps it only for what rutis does not do: it checks each row
against its `provides` snapshot and hands `workflowFiles` rows their packages. With configured runtime paths,
admission waits for each runtime's rows (`SessionRows::key(row)`); a runtime RuntimeManager acquires
is optional, so startup never waits for a download. `plugin.reload` is `Loader::reload(row)`; there
is no resolver cache.

rutis-loader's `LoaderPlugin` also serves the marked service `rutis/loader`. A foreign row that injects it
can `reload` a row and read `status`; `create` and `remove` are refused, because Dim configures no
editable layer, so PluginManager stays the only writer of rows.

## Appendix B: TypeScript and Python binding

Parts use `definePlugin` (in Python `define_plugin`, or a module with `inject`, `provides` and
`apply`) with `inject` and `provides`.

A plugin is one process row, in either language. Its `apply` registers each Session and branch part
with `partRegistry.registerDefinition(scope, name, definition)`, where `definition` is a
`definePlugin` value (in Python a `define_plugin` value; in Node also an unmodified Cordis 4 plugin)
that may close over what the plugin prepared. Its row injects `partRegistry`, and lists in
`instanceNames` the names its parts provide in each Session or branch (§2). The SDK's generated
declarations come from the catalog.

```json
{ "id": "goal", "name": "/abs/goal/index.ts", "inject": ["partRegistry"], "provides": {},
  "config": {} }
```

```ts
export default definePlugin({
  name: 'Goal',
  inject: ['partRegistry'],
  async apply(ctx) {
    const parts = ctx.use<Services['partRegistry']>('partRegistry')
    await parts.registerDefinition('session', 'GoalSession', session)
    await parts.registerDefinition('branch', 'GoalBranch', branch)
  },
})
```

A Python row names its module after the runtime's row, `py:<module>`; the App Server runs it in its
Python runtime when one is configured, importing the module from the configured project directory.
The Python runtime needs Python 3.12 or later.

```json
{ "id": "python", "name": "py:python_plugin", "provides": {}, "config": {} }
```

```python
from rutis import define_plugin

async def apply_branch(ctx, config):
    dispatcher = ctx.use("dispatcher")
    identity = await ctx.use("timelineIdentity").get()
    ...

branch = define_plugin(apply_branch, inject=["dispatcher", "timelineIdentity", "treeCommands"])

inject = ["partRegistry"]
provides = {}

async def apply(ctx, config):
    await ctx.use("partRegistry").registerDefinition("branch", "PythonBranchPart", branch)
```

```ts
// A Capability that rebuilds from history, then follows live events (plugin contract §5.2).
export default definePlugin({
  name: 'ExampleCounter',
  inject: ['dispatcher'],
  provides: { exampleCounter: { get: 'sync' } },
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    let count = 0
    const fold = (event: BranchEvent) => {
      if (event.type === 'input' && event.name === 'message') count++
    }
    await dispatcher.consume('example-counter', ['input/message'], delivery => {
      if ('live' in delivery) fold(delivery.live)
      else delivery.replay.forEach(fold)
    })
    ctx.provide('exampleCounter', { get: () => ({ count }) })
  },
})
```

The hand-written form stays valid: `listen` before `committed()`, then a rebuild from `local`
through that cutoff while the handler waits, skipping the live events it covered.

The repository's fixtures show both languages and both forms: `runtime/fixtures/branch-part.ts`
rebuilds by hand, `runtime/fixtures/python/python_branch_part.py` and `runtime/plugins/goal/branch.ts`
use `consume`.
