# Sessions and branches are plugins, not loader instances

Status: Draft.

Scope: Session, branch.
Issue: [#594](https://github.com/arcships/dim/issues/594).
Rules: [design rules](../architecture/rust-agent/principles.md), [ADR 002](../adr/002-plugin-scope.md),
[ADR 004](../adr/004-loader-plugin-management.md), [ADR 006](../adr/006-plugin-contract.md) D3, D4 and D6,
[How Dim uses rutis §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts).

Builds on: rutis 0.19.3, which the 0.19 upgrade ([#730](https://github.com/arcships/dim/pull/730)) pins
and has landed. It replaces the "one row per instance" draft of this design, and the row-handle
read [#645](https://github.com/arcships/dim/pull/645) added goes with it.
Decision: [ADR 015](../adr/015-scopes-are-plugins.md) (Proposed).

## 1. Scope and non-goals

A Session or branch is a loader instance today for one reason. When a part of another language is
mounted, rutis resolves the instance names it injects (`sessionMethods`, `toolRegistry`,
`dispatcher`, …) by finding the copy's instance in the loader's own instance table:

- `Definition::at` needs the loader's `PluginMounter` ("needs rutis-loader in the tree",
  rutis-bridge `runtime/definitions.rs:83`);
- the mounter calls `Loader::build_at`, which matches the copy's scope chain against the loader's
  instances (rutis-loader 0.17.1 `loader/mod.rs:287`, 0.19.0 `:331`).

Everything else an instance does is Dim's: the mounters mount parts with `ctx.plugin`, creation waits
for the required parts, closing goes through rutis's ownership of children. Being a loader instance
costs this:

- the mounters, the extension launcher and the extension plugins are rows of the instance, and
  creation finds them, and judges readiness, by scanning every loader entry
  ([#594](https://github.com/arcships/dim/issues/594));
- per-Session content (Skills contents, MCP connections) is a nested loader instance, created from
  the Session scope row through `SessionLoader`, because the loader creates an instance only from a
  row's context.

rutis 0.19.1 removes the reason ([rutis#948](https://github.com/arcships/rutis/issues/948),
[rutis#961](https://github.com/arcships/rutis/pull/961)). `Definition::at` takes a copy's instances
from its position in the plugin tree (`ctx.scope()`; rutis-loader 0.19.3 `loader/mod.rs:334`), and
`mark_scope(group)` is a public method on `Ctx` (rutis 0.19.3 `ctx.rs:1000`). A host mounts a plain
plugin with `ctx.plugin`, marks it as the scope of a group, and foreign copies inside it resolve that
group's names to it.

This design makes the Session and branch plain plugins that mark their group, and the per-Session
content plain plugins of the parts that own it. Nothing of a Session is a loader row.

Non-goals:

- **The Agent group.** It stays an instanced loader group: its row isolates the Agent's process values
  (`AgentBuilder::agent_row`), and `agent.create` places Agents as its instances
  (`dim-app-server/src/agents.rs:1297`). Nothing in it scans the loader's entries.
- **Names and the catalog.** The catalog's groups (`session`, `branch`), the naming scheme, the keys
  a part gets, and the `instanceNames` a foreign process row declares stay as they are. `Names` keeps
  its role and `Names::check`'s rule; only `Names::copy` takes the scopes' instances instead of a
  loader `Build`, and `named` refuses a Rust-typed instance name (§2.2).
- **The mounters' contract.** They call the generators, follow the registry and hand what they mount
  to creation (How Dim uses rutis §2 "Mounting").

## 2. Own contract

### 2.1 Now

On main at 08d78bca (rutis 0.17.1, with #666):

```text
agent group (instanced)
  AgentScope (AgentAnchor in the minimal host)
  session group (instanced)                    branch group (instanced, nested)
    SessionScope                                 BranchScope
    SessionExtension (when extensions exist)     required mounter
    required mounter                             optional mounter
    optional mounter
    branch group
    extension groups (SkillContentInstances, McpConnections, requested ones)
```

- Creation calls `create_instance`. The loader's `settle` awaits every row of the instance before it
  returns (rutis-loader 0.17.1 `loader/reconcile.rs:1020`), which is why the mounters do not wait in
  their own `apply`.
- Creation then finds the instance's rows by scanning `Loader::entries()`: to report the required
  rows' failures, to wait for them to be Active, and to let the optional rows settle
  (`dim-agent-runtime/src/assembly.rs:1597`, `:1609`, `:1615`). The assembly check scans the same
  way (`scope_rows`, `:2145`).
- Creation is bounded by `StartupProgress`: it gives up after no progress in its roots' subtrees for
  the startup timeout (`:37-118`, #666). Its roots closure finds the instance by scanning too
  (`locate`, `:1548`, `:1689`).
- The extension launcher creates a requested extension from its own row context (`:1231`). The
  Skills and MCP parts create their content through `SessionLoader`, which creates instances from the
  Session scope row (`skills/content.rs:160`, `mcp/publication.rs:613`).

### 2.2 Target

```text
agent group (instanced)
  AgentScope (or AgentAnchor)
    └─ SessionScope (mark_scope "session")
         ├─ required mounter ── required parts
         │     ├─ Skills ── SkillContent …
         │     └─ McpSession ── McpServer …
         ├─ optional mounter ── optional parts
         ├─ requested extension plugins
         └─ BranchScope (mark_scope "branch")
              ├─ required mounter ── required parts
              └─ optional mounter ── optional parts
```

Skills and McpSession are required parts in the profiles that install them (`require_part`); the
tree shows where their content sits.

- **The scope is a plugin.** The Agent mounts a Session's `SessionScope` with `ctx.plugin` from its
  anchor. Its `apply` first calls `ctx.mark_scope("session")`, then provides the Session's services
  at keys qualified with `ctx.instance()`, as today. The scope's own fiber id is the Session's instance:
  the kernel admits instance keys for every fiber below it (rutis `ctx.rs` `in_instance`), and
  `ctx.scope()` reports it to every copy below it. The Session's owner (`SessionAssembly`) is the
  plugin's own field, not a value given to `create_instance().with`. A branch is the same:
  `BranchScope`, mounted from the Session scope's context, marks `"branch"`.
- **The scope mounts the rest.** After providing its services, the scope mounts the required and the
  optional mounter, and the requested extension's plugins (below), with `ctx.plugin` as its own
  children. It hands their views to creation, as the mounters hand the copies they mount, and returns.
  It waits for none of them in `apply`: creation is the one place that waits.
- **Names.** A Rust copy's keys are qualified with its scopes' instances: the Session scope's
  `ctx.instance()` and, in a branch, the branch scope's. A mounter gives `Names` that group →
  instance list instead of a loader `Build`, and `Names` qualifies keys itself, as its composition
  check already does (`Names::check`). The catalog still says which group a name belongs to.
  - `key::<T>()` gives `Key::<T>::of().instance(id)`, as today (`names.rs:195`).
  - `named(name)` gives `host_key_in(name, id)` for a shared instance name, which is the key the
    catalog gives it today (`host_key` is `Key::dynamic(name)`, rutis-bridge 0.19.3
    `session/services.rs:48-54`). A Rust-typed instance name is read with `key::<T>()`, and `named`
    refuses one, naming `key::<T>()`. Every `named` caller today reads a shared name.
- **Foreign copies** are mounted with `Definition::at` under a mounter, as today. In 0.19.3 it takes
  the copy's instances from `ctx.scope()`, so `session` and `branch` names resolve to the enclosing
  scopes, and each copy is isolated per instance as today.
- **Creation waits in three steps.** Without the loader's `settle`, nothing waits unless creation
  does:
  1. The scope must be Active. A failed `apply` is the creation's error.
  2. The views the scope handed: the required mounter must be Active, the optional mounter must
     settle, and the requested extension's plugins must be Active. The required mounter is Active
     only once every required part is registered and mounted (`parts.rs:461-480`). A required part
     that fails to build fails the required mounter (`parts.rs:378-381`), and that failure fails
     creation with its own error. A failed optional mounter stays local, as an optional row's
     failure does today.
  3. Only then does creation read what the mounters handed (`Opened`, which a mounter fills at the
     end of its `apply`, `parts.rs:512-517`): required copies must be Active, optional copies must
     settle.

  `StartupProgress` bounds the three steps as it bounds creation today. Its root is the scope view's
  id, known as soon as `ctx.plugin` returns, so nothing has to find the instance: `locate` goes.
  When `StartupProgress` gives up, or on cancellation, creation shuts the scope's view down. The task
  that disposed of a creation still running, `rows`, `scope_entries` and `scope_rows` go too.
- **Requested extensions.** `Agent::create_session_with_extension(config, name, value)` keeps its
  contract: the plugins registered for `name` (`AgentBuilder::session_extension_part`) are built from
  `value` with the Session's `Names` and must be Active before the Session publishes. The Session
  scope builds and mounts them as its children and hands them to creation as required (step 2). A
  value of the wrong type fails creation; a plugin that stays Pending holds it until
  `StartupProgress` gives up.
  The launcher row, `ExtensionRequest` and the extension groups go. An extension name is no longer a
  loader group, so it can no longer shadow a row, and the "reserved extension group" check goes.
- **Per-Session content is its part's children.** Each Skills content and each MCP connection
  generation is a plain plugin its part mounts with `ctx.plugin` from its own context, as ADR 006 D3
  "Inside a part" already describes. The private immutable value (`ContentConfig`,
  `ConnectionConfig`) is the plugin's constructor argument, not a loader value. The part closes them
  as it does today: Skills' effect closes every content; McpSession releases its bindings, and a
  generation no lease uses closes (ADR 004). They also close with the part. Where an owner awaited
  `create_instance` and read its report, it awaits the view `ctx.plugin` returns. `SessionLoader`
  goes, and `McpOwner::new` takes only a context.
- **What a scope's record holds.** `Loaded::children` holds the views the scope handed (its mounters
  and the requested extension's plugins); `Loaded::required` holds the required ones among them,
  views rather than row ids; `Loaded::parts` keeps the required copies.
  - Session diagnostics are the Session scope's subtree without its branches' subtrees, which report
    separately, as today. Branch diagnostics are the branch scope's subtree. Both use the existing
    `subtree`; `retain_scope`'s filter for mounter modules goes.
  - Closing is the scope's shutdown, and its children close with it. `shutdown_scope` drops the
    errors already reported by the scope, the handed views and the copies, as today, so a failed
    mounter's error does not come back as a cleanup failure.
  - The assembly check reads each live scope's record: its required handed views and its required
    copies must still be Active, and an unawaited child is reported as today.
- **Install.** `AgentBuilder::install` registers no Session, branch, mounter or extension module and
  returns no Session group. The Agent group row holds the Agent's own rows (`AgentScope` or
  `AgentAnchor`, process values). The scopes construct their mounters.

### 2.3 Observable changes

- **Fiber tree:** a Session is a child of its Agent's anchor row (`AgentScope`, or `AgentAnchor` in
  the minimal host), a branch of its Session scope, and the mounters of their scope. Skills contents
  and MCP connections sit under their parts. The `session` and `main` subgraphs of the generated
  graphs regenerate. The `SessionLoader` service and its edges go; no other service or edge changes.
- **The anchor must not reload.** Today a Session is a sibling of the anchor in the Agent group;
  now it is the anchor's child. A reload of the anchor would dispose every Session of that Agent
  without `Agent::close`'s settlement. It does not reload today: `AgentScope` depends only on the
  directory and the loader, which do not reload (`dim-app-server/src/agents.rs:1677`). A dependency
  added to it, or to `AgentAnchor`, must keep that.
- **Loader entries:** `Loader::entries()` no longer lists any Session, branch, mounter or extension
  row. PluginManager lists process rows only, so `plugin.list` does not change.
- **Creation cost:** nothing scans the loader's entries, which is #594's own proposal. `xtask measure`
  at 1, 10, 100 and 1000 Sessions records the effect.
- **`ctx.row()` of a part:** a part's nearest row was its mounter row; it is now the Agent's anchor
  row. `MethodTable::register` records it (`dim-agent-capabilities/src/methods.rs:525`). The Session
  method router looked a withdrawn namespace's row up among the rows of this Session's instance
  (`dim-app-server/src/methods.rs:218-235`). There are none now, so that lookup goes, and a withdrawn
  Session namespace answers `service_reloading` until it is registered again. The row it found was a
  mounter row, which is Active while the Session lives, so the answer is the same as today.
  Management namespaces keep their lookup: they are registered by process rows.
- **Errors:** the "reserved extension group" build error goes. Every other error keeps its kind and
  text.

## 3. Events and state

None. No Timeline fact, bus event or persisted state changes. Instance ids are runtime handles, not
durable identities, so Session and branch ids are unaffected.

## 4. Dependencies

- **rutis 0.19.3:** `ctx.plugin`, fiber views and states, `ctx.scope()`, and the public `mark_scope`.
- **rutis-loader 0.19.3:** `Definition::at` through its mounter, by the copy's position in the tree;
  `ServiceCatalog::key_in`, to tell a name's group; the Agent group as an instanced group.
- **rutis-bridge:** `host_key_in`.
- The part registry and the mounters, as in How Dim uses rutis §2.
- The 0.19 upgrade (#730), which pins 0.19.3 and has landed.

## 5. Cross-module requirements

None. rutis 0.19.3, which #730 pins, has everything this design uses: a foreign copy resolves its
instances from its position in the tree, and a host marks its own scopes with `mark_scope`
([rutis#948](https://github.com/arcships/rutis/issues/948), raised for this design and released in
0.19.1). Rust parts qualify their keys themselves.

## 6. Rule check

| Rule | Answer |
| --- | --- |
| §1 (Capabilities) | No Capability is added or changed, and no part is added. The scopes and the mounters are mechanisms (ADR 006 D6: "the scope roots") |
| §4 rule 1 (command events) | Not applicable: no module changes state another module owns |
| §4 rules 2, 3, 5, 6, 9 | Not applicable: no handover, `before` transform, batch, context path or Timeline fact changes |
| §4 rule 4 (core mechanisms without business meaning) | Creation and the scope treat every part and extension plugin alike. Skills contents and MCP connections are their parts' business; no core code knows them |
| §4 rule 7 (public events and services) | No service is added; `SessionLoader` goes. Creation reads only the views the scope and the mounters hand it, not the loader's table |
| §4 rule 8 (infrastructure unchanged by default) | The need is the assembly's own (#594's creation cost), and it closes inside the assembly, using rutis 0.19.3 as #730 pins it. Nothing is asked of rutis (§5) |
| ADR 002 D1–D5 | Parts stay in their Session or branch. No wiring module, configuration or language contract changes; foreign parts still resolve their names |
| ADR 004 | Its ownership stands: connections belong to a Session, closing a Session closes only its generations, configuration stays private and immutable, and no second loader or lifecycle manager comes in. Its mechanism, a connection generation as an instanced group with the loader owning disposal, is superseded by [ADR 015](../adr/015-scopes-are-plugins.md) |
| ADR 006 D3 | The instance still mounts the parts registered for it, from its scope. The scope and the mounters are the instance's mounting mechanism, built by Dim, with no configuration, enablement or reload of their own, so "a plugin is one row" does not cover them. "Inside a part" now holds literally for McpSession and Skills |
| ADR 006 D4 | Names resolve to the nearest enclosing instance of their group, now the nearest scope marked with it, through the same catalog |

## 7. Verification

- The Session and branch flows of `dim-agent-capabilities` and `tests/integration`, with their
  assertions unchanged:
  - a required part registered late holds creation until it is Active;
  - a required part that fails to build fails creation with its own error;
  - a failed optional mounter or part stays local, and optional parts settle before publication;
  - registrations follow open instances.
- The foreign part flows (SessionTitle, Scheduler, Workflow and Goal, and the Node-backed App Server
  flows) on rutis 0.19.3: copies resolve `session` and `branch` names inside their scope.
- Requested extensions (`tests/integration/tests/extensions.rs:884-974`): Active, Failed, Pending and
  a wrong value type keep their outcomes. `transport_flow/reload.rs` drops its "reserved extension
  group" assertion.
- Skills contents and MCP connections: created, replaced and retired as today, and closed with their
  Session. ADR 004's flows (independent connections, retained bindings, Session close, cross-Session
  disconnect, cleanup reporting) are unchanged.
- `Session::diagnostics()` lists the scope, the mounters and the parts and no branch;
  `branch_diagnostics` lists the branch's. `Agent::check_assembly()` reports an unawaited child, as
  today.
- A withdrawn Session namespace answers `service_reloading`.
- `xtask graph`: only the `session` and `main` subgraphs change. `xtask measure` before and after.

## 8. Documentation to update with the implementation

- [How Dim uses rutis §2](rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts) "Mounting":
  "holds two mounter rows" becomes "mounts two mounters as the scope's children", and "It does not
  wait in its own `apply`, because the loader's instance creation waits for every Loading row"
  becomes "Creation waits for the mounters, then for what they hand". Appendix A.2's `SessionLoader`
  sentence goes.
- [Plugin contract](plugin-contract.md): the scope row of the terms table and the instance tree
  (`session ⟨instanced⟩`, `branch ⟨instanced⟩`).
- [single-tree](../architecture/rust-agent/single-tree.md): the tree (`session`, `McpConnections`,
  `branch`), the `create_instance` note, the `rutis-loader` row, the MCP generations row, the
  `InstanceId` note and the assembly section's required rows.
- [Skills](../architecture/rust-agent/plugins/Skills.md): `S:SessionLoader` and the
  `SkillContentInstances` template; [MCP OAuth](appserver/mcp-oauth.md): the `McpServer` row.
- [single-tree](../architecture/rust-agent/single-tree.md) `:111`:
  - "请求的 Session 扩展由 `SessionExtension` 行装载，该行显式注入 `SessionIdentity`" and the sentence after it
    become: the Session scope mounts the requested extension's plugins as its children and hands them
    to creation as required, within the bound `StartupProgress` sets;
  - "嵌套实例归 Session 分组所有，不归创建它的行所有" becomes: a Session's content belongs to the part
    that mounts it.
- [Plugin pages index](../architecture/rust-agent/plugins/README.md) `:15-17` and
  `xtask/src/plugins.rs:43-55`: the excluded infrastructure list drops `SessionExtension`, `session`
  and `branch`; the scopes and mounters stay.
- [Session TypeScript](session-typescript.md) `:20`: "loader instances isolate their services"
  becomes "the Session and branch scopes isolate their services".
- [ADR 005](../adr/005-observability.md) `:101` (Proposed): the `SessionExtension` mechanism it lists
  as unchanged is removed.
- `crates/dim-app-server/README.md:205-214`: `McpOwner::new(ctx)`, no `standalone_template` (the
  standalone `McpConnections` template, `mcp/publication.rs:216-226`, goes) and no "declared Session
  loader".
- `AgentScopePlugin`'s doc comment (`dim-app-server/src/agents.rs:1677`): its Sessions are its
  children, which is why it must not reload.
- The Chinese mirrors: [How Dim uses rutis](../zh/design/rutis-usage.md) `:359` and
  [Plugin contract](../zh/design/plugin-contract.md) `:52`.
- The generated graphs, through `xtask graph`.
- #645 landed on 0.19.2 as a follow-up that reads current row handles through the published
  `Loader::instance_rows` ([rutis#941](https://github.com/arcships/rutis/issues/941)), with no
  behavior change. This design still removes the instance rows that read relies on, so that code
  goes when this lands.

## Appendix A. Rust binding

- `SessionScopePlugin::new(owner)` and `BranchScopePlugin::new(owner)` hold their assembly. `apply`
  calls `ctx.mark_scope`, provides the services, and builds the two `PartMounter`s from a
  `MountSite` that holds its instances (`session` → the Session scope's `ctx.instance()`, and in a
  branch `branch` → its own). It mounts them (and, for a Session, the requested extension's plugins)
  with `ctx.plugin` and records the views in the owner's `Loaded`.
- `Names::copy(catalog, instances)` takes that group → instance list in place of a `Build`.
  `key::<T>()` qualifies `Key::<T>::of()` with the instance; `named` gives `host_key_in(name, id)`
  for a shared instance name and refuses a Rust-typed one.
- Session creation mounts `host.plugin(SessionScopePlugin::new(owner))`; branch creation mounts
  `session.ctx.plugin(BranchScopePlugin::new(owner))`. One `wait` function runs the three steps of
  §2.2 on the returned view and the owner's record.
- `AgentBuilder::install` keeps only the process values' registration and returns no group; the App
  Server's Agent group row (`agents::group_row`) takes no Session row.
- `session_extension_part(name, plugin, make)` stays, read by the Session scope for requested
  extensions. Skills and MCP no longer call it: `ContentPlugin(config)` and
  `ConnectionPlugin::new(config)` are mounted by their owners with `ctx.plugin`.
- These go: `create_instance` in `assembly.rs`, `locate`, `rows`, `scope_entries`, `scope_rows`,
  `SessionExtensionPlugin`, `ExtensionRequest`, `SessionLoader` and its catalog name, and the loader
  lookup in `session_provider`.
