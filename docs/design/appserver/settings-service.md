# Settings service

Status: Design approved; implemented. Rust side (S8b part 1, §12), the TypeScript owner path
(part 2), event delivery to TypeScript parts (part 3), sources registered at run time by their
owners (part 4, [PluginManager §9 R1](../plugins/PluginManager.md#9-requirements-raised-to-mechanism-owners)).
The shared `credentials` service is withdrawn (§1.2). Roadmap item S8b ([#118](https://github.com/arcships/dim/issues/118)).
Builds on S8a ([#54](https://github.com/arcships/dim/issues/54), PR [#90](https://github.com/arcships/dim/pull/90));
implementation depends on C4 ([#63](https://github.com/arcships/dim/issues/63)).
Overview: [§6.4 Configuration](../overview.md#64-conventions). Scope:
[ADR 002](../../adr/002-plugin-scope.md) D4 and D5 (process, inside the existing Settings plugin);
[ADR 004](../../adr/004-loader-plugin-management.md) D4.

## 1. Summary

Settings serves sources owned by plugins that are not written in Rust. Such a plugin registers
each source with Settings when it starts ([ADR 007](../../adr/007-plugin-configuration.md) D5):
the source name, its files, a JSON Schema (with `default` values) and its user-only fields
(§3.1). Settings then offers three things over JSON documents:

- the process service `settings`, which the owner uses: `register`, `read`, `update` (CAS on
  the revision) and `projects`;
- the client management methods `settings.read` and `settings.update`;
- the same rutis events as S8a, `settings.changed` and `settings.project_authorized`. Their
  payloads become serializable. There is no `subscribe(source)`.

There are two write paths. A client write (`settings.update`) may change any field. An owner write
(the `settings` service) may not change a user-only field. A write the owner makes on behalf of
its tool is an owner write, so it is rejected when it changes a user-only field.

Settings gives no field a meaning, with one exception: a member whose schema has the JSON Schema
annotation `writeOnly: true` is a secret toward the agent (§1.2). A secret is otherwise an ordinary
field of its owner's document, and the owner decides whether and how to store, encrypt or look it
up.

`BusinessDocument` stays the Rust binding; a Rust owner registers its document types the same
way (§4.5). Rust sources keep their owners' management methods. F2's `permission.rules`/`permission.set_rules` stay (§8.1).

No new plugin, no Timeline event, no change to Timeline, Dispatcher, rutis or assembly.

The same Settings provider supplies generated `workflowFiles` for definition
directories. It reads the existing project authorization map and the original
process caller’s declared package scope, without exporting FileStorage or raw
root indices. See the [native definition-file contract](../p6-p7-bindings.md#3-workflow-definition-files).

### 1.1 What this does not do

- No `subscribe(source)`. The issue text lists it, but S8a moved change notification to the rutis
  event bus (issue comment). Settings keeps no watch channel of its own.
- No generic client access to Rust sources (`mcp`, `mcp-projects`, `mcp-plugins`, `skills`, `permission`).
  Their validation is Rust code that Settings cannot run on client JSON.
- No source discovery method and no settings UI. See §10.
- No secret store, credential service or field-name rule (§1.2). The one marker is the schema's
  `writeOnly` annotation.
- MCP's inline tokens, `env` and `headers` stay where they are (ADR 004 D4).

### 1.2 Decision: secrets belong to their owner

Whether a plugin keeps a secret, and how it stores or encrypts it, is that plugin's business,
in Rust or TypeScript. Infrastructure does not turn one owner's need into a shared service
(principles §4 rule 8). The existing owners already work this way: MCP OAuth keeps its tokens in
its own 0600 `FileStorage` record ([MCP OAuth](mcp-oauth.md)), MCP keeps inline tokens in its
source, and Provider keeps inline credentials and its own optional `secretRef` host.

An earlier revision of this design added a process service `credentials.resolve(owner,
reference)`, required `*Ref` fields to be non-empty strings, and moved Provider's secret host onto
that service. No product plugin provided the service, so every owner that used a reference
depended on a store nobody owned. That service and the `*Ref` rule are withdrawn:

- A secret is an ordinary document field. The owner's schema states its rules (for example
  `minLength`). A field meant for the user only is declared in `userOnly` and kept in the user
  file, not a project file, so it is not committed with a project.
- Storage and reads: the file-backed profile writes the user document with mode 0600 on Unix.
  The native profile stores it in SQLite as `user/<source>`, not in a separate 0600 file. Its
  boundary is the product home: new Unix directories use 0700, Windows inherits the parent DACL,
  and existing directory permissions are preserved. `settings.read` serves local connections
  only (§7), so local clients can read the secret; remote and paired clients cannot. The owner
  never places it in tool definitions, results, events, diagnostics, errors or logs.
- `writeOnly: true` on a member reachable through `properties` marks a secret. It does not
  change validation. The agent's path to a document, the `plugin` tool's `config` command
  ([PluginManager §6](../plugins/PluginManager.md#6-interfaces-summary)), never returns its value:
  the member is left out and its JSON Pointer is listed as set when it holds a value; and that
  command refuses to write a member that is, or contains, a `writeOnly` member. Clients are
  unchanged: local `settings.read` and `settings.update` read and write it as any field (§7). An
  owner declares `writeOnly` on every credential in its schema, in addition to `userOnly`.
- An owner that wants a keychain or encryption implements it inside its own plugin and keeps
  only what it chooses in its document.
- Provider keeps its own `ProviderSecretHost` / `ResolveProviderSecret` (`provider/secrets.rs`),
  as before S8b. It is Provider's extension point, not a shared contract.

## 2. Structure

```
 Process scope
 +-------------------------------------------------------------------------+
 |  Settings (existing plugin)                                             |
 |   SettingsService                                                       |
 |     registered sources, each until its registering plugin unloads:      |
 |       BusinessDocument types (Rust owners)                              |
 |       declared sources: schema, defaults, files, user-only pointers     |
 |   named service `settings` (HostDispatch)  <-- owner writes             |
 |   managementMethods `settings`             <-- client writes            |
 |   emits settings.changed on settings/<source>,                          |
 |         settings.project_authorized on settings/projects                |
 +------^-------------------------------^----------------------------------+
        | register/read/update/projects | settings.read / settings.update
        |                               |
  TypeScript owner (rutis runtime row)  Rpc  <--  local client
```

Dependencies point to the core. Settings depends on no plugin and names no owner. A TypeScript
owner depends on `settings`.

## 3. Data and state

### 3.1 Source registration

Every owner, built-in Rust and TypeScript alike, registers its sources from its own process row's
`apply`, a TypeScript owner with `settings.register(declaration)` (§4.2) and a Rust owner with
`SettingsService::register::<D>` (§4.5). Neither the App Server's assembly nor PluginManager
declares sources. A registration belongs to the plugin that made it and is withdrawn when that
plugin unloads (turned off, removed or reloaded); a reload registers again from the new
generation. The stored documents stay in the home whether or not their source is registered: an
unregistered source is `unknown_source` on every path, and its document is served again when an
owner registers it.

A declaration, as the owner's code holds it:

```json
{
    "source": "web",
    "userFile": "web.json",
    "projectFile": null,
    "schema": {
      "type": "object",
      "properties": {
        "searchService": {
          "type": "object",
          "properties": {
            "kind": { "type": "string", "minLength": 1 },
            "endpoint": { "type": "string", "format": "uri" },
            "apiKey": { "type": "string", "minLength": 1 }
          },
          "required": ["kind"],
          "additionalProperties": false
        },
        "allowPrivateHosts": { "type": "array", "items": { "type": "string" }, "default": [] },
        "proxy": { "type": "string", "format": "uri" }
      },
      "additionalProperties": false
    },
    "userOnly": ["/searchService", "/allowPrivateHosts"]
}
```

| Field | Rule (checked once, at registration) |
| --- | --- |
| `source` | `[a-z0-9_-]`, as for Rust sources. Unique among the registered sources of the process, Rust sources included |
| `userFile` | Same rule as `BusinessDocument::USER_FILE` (a plain filename in the home root). No two registered sources share a user file |
| `projectFile` | Optional, same rule as `PROJECT_FILE`. Present means the source also has project scope. This is the "allowed scopes" declaration. No two registered sources share a project file |
| `schema` | JSON Schema (draft 2020-12). It must compile, and its root must be `type: object` |
| `userOnly` | JSON Pointers (RFC 6901) to object members. Each must name a member reachable through `properties` |

The default document is `{}` with defaults applied (§3.2). It must be valid against the schema,
or the registration fails. So a missing file always reads as a valid document. A refused
registration fails the registering call, and so the caller's `apply`.

### 3.2 Documents, validation and defaults

A declared source holds one JSON object. Settings handles it at its boundary, as S8a does for
Rust documents:

- **Defaults**: for every object whose schema has `properties`, a missing member with a `default`
  is filled in. This recurses through `properties` only (not through `$ref`, `allOf`, `anyOf`,
  `oneOf`), the same reach as serde's `#[serde(default)]` on nested structs.
- **When**: on every write, and when reading a persisted file or native row. A missing file is
  `{}` with defaults applied. The written and returned document is the filled one.
- **Validation**: after defaults, against the schema. Settings applies no rule of its own to
  member names or values (§1.2).
- Consumers use the document as delivered. They do not validate again or add defaults. A rule
  that JSON Schema cannot express (such as a CIDR) is the owner's diagnostic at use time.

### 3.3 Persistence and revisions

Unchanged from S8a. User scope uses the user file, or the additive native row `user/<source>` under
the v13 `settings_documents` contract. Project scope uses the project file and content CAS. Revisions are the same opaque
strings (`missing`, a SHA-256 of the file, or `native:user/<source>:<version>`). No format change.

### 3.4 Runtime state

Settings keeps the registered sources in memory, declarations compiled, each until its registering
plugin unloads. Nothing else is new. Nothing enters the Timeline.

## 4. Interfaces

### 4.1 Common shapes

| Shape | JSON |
| --- | --- |
| `Scope` | `"user"` or `{"project": <root>}`. `root` is the project root number Settings assigns in this process (`SettingsScope::Project`). It is not a durable identity and is never persisted |
| `Snapshot` | `{revision: string, document: object}` |
| `Error` | `{code, message}` |

### 4.2 Service `settings` (owner path)

Process scope, no instance key, provided by Settings.

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `register` | async | a declaration (§3.1) | nothing; rejects when refused |
| `read` | async | `{source, scope: Scope}` | `Snapshot` |
| `update` | async | `{source, scope: Scope, expectedRevision, document}` | `Snapshot` (the stored document) |
| `projects` | sync | `{}` | `{projects: [{root, cwd}]}`: the authorized project roots now |

- `register` is a registration of the calling row, withdrawn when that row unloads. It is called
  from the row's own `apply`; a refusal (§3.1) rejects the call rather than returning an
  envelope, so it fails that `apply` as every owner's `register` does.
- `update` is CAS: it fails with `conflict` unless `expectedRevision` is the current revision.
- `update` is an owner write. It fails with `user_only_field` when the stored document and the new
  one differ at any `userOnly` pointer (a value changed, added or removed). The comparison uses
  the documents after defaults.
- `projects` is what an owner reads first. Afterwards it follows `settings/projects` (§4.4).
- Cancellation: the caller drops the call. A write that has already committed stays committed,
  as in S8a.
- Errors of `read`, `update` and `projects` (§6): `unknown_source`, `invalid_scope`,
  `invalid_document`, `user_only_field`, `conflict`, `unsynced`, `failed`, `closed`, `cancelled`.

Only `register` uses the caller's identity, to withdraw the registration with the calling row.
Ownership comes from the registering row: each source has one owner, and only that package is
written against it. This is the trust model of in-process plugins
(AGENTS.md rule 11): dependency declarations are not a sandbox.

### 4.3 Management methods `settings.*` (client path)

Namespace `settings`, which `SettingsMethods`, a process part that depends on Settings, registers
with `managementMethods` (§4.5). They are never served to paired devices (overview §6.2 rule 9).

| Method | Request | Reply | Errors |
| --- | --- | --- | --- |
| `settings.read` | `{source, scope}` | `{revision, document}` | invalid params (bad scope), `settings_unknown_source`, `settings_failed` |
| `settings.update` | `{source, scope, expectedRevision, document}` | `{revision, document}` | invalid params (bad scope, schema violation), `settings_unknown_source`, `settings_conflict`, `settings_unsynced`, `settings_failed` |

- Only registered declared sources are served. A Rust source (for example `mcp`) is
  `settings_unknown_source`.
  Its owner's methods stay the way to change it.
- A client write may change user-only fields. There is no other difference from the owner path.
- `closed` maps to the existing retryable reloading error.

### 4.4 Events

Unchanged channels and names from S8a. The payloads become serializable data:

| Event | Channel | Payload |
| --- | --- | --- |
| `settings.changed` | `settings_changes(source)` = `settings/<source>` | `{source: string, scope: Scope, revision: string}` |
| `settings.project_authorized` | `project_authorizations()` = `settings/projects` | `{root: number, cwd: string}` |

A declared source emits `settings.changed` exactly as a Rust source does, after the commit, for
both write paths. The payload never carries the document. A consumer reads once, then follows the
events.

A part in any runtime listens to these channels on the bus (§10 question 2). A TypeScript part
registers `ctx.on('settings/<source>', (payload) => …)` for each source it uses and
`ctx.on('settings/projects', …)`, the payload being the JSON above as the one argument.

- A part listens to the sources it uses, Rust sources included. The payload never carries a
  document, so this exposes nothing a part could not read.
- Events of one channel start in commit order. A slow part delays neither the next event nor
  Settings' other listeners.
- No replay: a part reads once when it starts, then follows the events, as a Rust consumer does.
  An event emitted before a part's row loaded (for example its own Session's project
  authorization during creation) is not delivered to it.
- A failed delivery (a throwing listener) goes to the rutis error sink and does not stop later
  events.

Reading at use time remains valid for owners that do not need to react to changes (F2's
Permission does).

### 4.5 Rust binding

In `dim-app-server::settings` (S8a types, changed only where noted):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsScope { User, Project(usize) }            // + Serialize/Deserialize

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChanged { pub source: String,           // was &'static str
                             pub scope: SettingsScope, pub revision: String }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAuthorized { pub root: usize, pub cwd: PathBuf }   // + Serialize/Deserialize

/// A declaration (§3.1), as `register` receives it.
pub struct SourceDeclaration { pub source: String, pub user_file: String,
                               pub project_file: Option<String>, pub schema: serde_json::Value,
                               pub user_only: Vec<String> }

pub enum Writer { Owner, Client }

impl SettingsService {
    /// Registers `D` (or a declaration) until the plugin owning `ctx` unloads.
    pub fn register<D: BusinessDocument>(&self, ctx: &Ctx) -> rutis::Result<()>;
    pub fn register_declared(&self, ctx: &Ctx, declaration: SourceDeclaration) -> rutis::Result<()>;
    pub async fn read_declared(&self, source: &str, scope: SettingsScope,
                               cancel: &CancellationToken) -> Result<SettingsSnapshot<Value>>;
    pub async fn update_declared(self: &Arc<Self>, source: &str, scope: SettingsScope,
                                 expected: &str, document: Value, writer: Writer,
                                 cancel: &CancellationToken) -> Result<SettingsSnapshot<Value>>;
}
```

- Both registrations are effects of the plugin owning `ctx`, withdrawn when it unloads, and are
  checked under one registry state (§3.1). `register_declared` compiles the schema with
  `jsonschema`. The process owners register in their `apply`: Mcp (`mcp`, `mcp-projects`, `mcp-plugins`),
  SkillsCatalog (`skills`), PermissionRules (`permission`) and CollaborationModelPolicy
  (`collaboration`). Readers in other plugins only read; `read::<D>`/`update::<D>` of a type that
  is not the registered one for its name is `UnknownSource`. The `settings` service's
  `register` calls `register_declared` with the calling row's context.
- The file and native paths are shared with `BusinessDocument`, keyed by source name instead of
  `D::NAMESPACE`. CAS, the gate, leases, the unsynced case and `publish` stay as they are.
- `SettingsError` gains `UnknownSource`, `UserOnlyField(pointer)`, `InvalidScope` and
  `Document(message)`. A written document that breaks the schema or is not an object is `Document` with the validator's message (the instance path, never the value); the
  client path maps it to invalid params. A persisted document that breaks the schema stays
  `Invalid` (`invalid_document` / `settings_failed`).
- The `settings` service is a `HostDispatch` under `host_key("settings")`, and assembly registers
  `settings` in the `ServiceCatalog`, as `pluginFiles` is done today. Replies use
  the envelope of `sessions`: `{ok: true, value}` or `{ok: false, error: {code, message}}`.
- The `ManagementMethods` for `settings` come from `SettingsMethods`, a process part that depends
  on Settings (module `settings_methods`), the way PermissionRules owns `permission`. Settings
  itself cannot own a namespace: Rpc reaches Settings through Agents, Rpc may not reach a
  management owner (methods contract §3.2), and the module boundary keeps Settings persistence
  infrastructure (ADR 004 D4, `xtask check`). Assembly lists the `SettingsMethods` row as the
  owner of `settings`.
## 5. Flows

### 5.1 Owner reads and updates

```
 TS owner                 settings (HostDispatch)      SettingsService            storage
   | read {web, "user"} ---->| decode request           |                          |
   |                         | read_declared ---------->| file / user/web row ---->|
   |                         |                          | defaults, schema check   |
   |<-- {revision r1, document} -------------------------|                          |
   | update {web, "user", r1, doc'} ->| update_declared(Owner)                       |
   |                         |                          | defaults, schema         |
   |                         |                          | user-only diff vs current|
   |                         |                          |   differs: user_only_field
   |                         |                          | CAS r1 ------------------>| replace
   |                         |                          | emit settings.changed {web, "user", r2}
   |<-- {revision r2, document} -------------------------|                          |
```

The user-only diff is checked under the same lease (or native transaction) as CAS, against the
document it replaces.

### 5.2 Client edits a user-only field

```
 Client            Rpc              Settings (managementMethods)
   | settings.read {web, "user"} ---->| {r2, doc}
   | settings.update {web, "user", r2, doc with searchService} -> update_declared(Client)
   |<-- {r3, doc}                     | emit settings.changed {web, "user", r3}
 TS owner (its tool asks to change searchService)
   | update {web, "user", r3, doc with another searchService} -> user_only_field, nothing written
```

### 5.3 Registration

```
 owner row apply                  settings (HostDispatch)      SettingsService
   | register(declaration) ------->| caller = calling row       |
   |                               | register_declared -------->| parse (unknown fields rejected)
   |                               |                            | check name, files, pointers
   |                               |                            | compile schema; {} with defaults valid
   |                               |                            | name or file registered? refuse
   |                               |                            | record; withdrawal = caller's unload
   |<-- resolved (or rejected: the row's apply fails) ----------|
 owner row unloads -> its sources are withdrawn; their documents stay in the home
```

Settings is built whenever there is a product or target home, as before; a plugin that registers
a source needs it.

## 6. Errors

| Code (service / wire) | When | Retry |
| --- | --- | --- |
| `unknown_source` / `settings_unknown_source` | No source of that name is registered, or (on these paths) it is a Rust source | After its owner registers it |
| `invalid_scope` / invalid params | Project scope for a source without `projectFile`, or a root not authorized | After authorizing the project |
| `invalid_document` / invalid params | Schema violation or a non-object document | No; nothing written |
| `invalid_document` / `settings_failed` | The persisted file or row is invalid (on read) | After the user repairs it |
| `user_only_field` | An owner write changes a user-only field | No; the user changes it through the client |
| `conflict` / `settings_conflict` | Stale `expectedRevision` | Read and retry |
| `unsynced` / `settings_unsynced` | The new content is in place and announced, but could not be synced | Read before retrying |
| `failed` / `settings_failed` | Storage, persistence or worker failure | Read before retrying |
| `closed` / reloading | Settings is unloading | Yes |
| `cancelled` | Call dropped or cancelled | Caller's choice |

## 7. Security

- Management methods are local only; paired devices never reach `settings.*` (overview §6.2 rule 9).
- User-only fields protect user decisions from the owner's own writes, including writes made for
  the model's tool. They are not a boundary against a hostile plugin. In-process plugins are
  trusted and not sandboxed (AGENTS.md rule 11, ADR 003).
- Project files keep their own authority (ADR 001). A declared project file is read only for an
  authorized root, as for Rust sources. Settings is not a security setting, and no Settings
  document confers approval (ADR 003).
- Error messages and validator messages carry paths, never values. A secret in a document is the
  owner's to protect (§1.2): keep it in the user source and in `userOnly`. File-backed and native
  SQLite profiles retain the storage boundaries in §1.2; `userOnly` restricts writes, not reads.
  `settings.read` returns it to local clients and to no remote or paired client.
- Settings documents never enter the Timeline. Replay and fork do not restore or widen them.

## 8. Relationship to existing parts

| Part | Relationship |
| --- | --- |
| S8a Settings | Supplement: declared sources, the named service, the `settings` namespace, serializable event payloads. `BusinessDocument`, CAS, native rows and channels are unchanged |
| MCP, Skills | Unchanged. Their sources stay Rust documents with their own management methods |
| Permission rules (F2) | Unchanged; see §8.1 |
| Provider | Unchanged. Its inline credentials and its own optional `ProviderSecretHost` stay (§1.2) |
| ADR 004 D4 | It says Settings publishes no management methods until S8b. S8b adds the `settings` namespace, limited to declared sources. ADR 004 is updated with the implementation |
| C4 (TypeScript bridge) | Its rows register their sources from their code (§3.1) and listen to the Settings channels themselves (§4.4) |
| PluginManager | Declares no sources: a plugin's business configuration is its own ([ADR 007](../../adr/007-plugin-configuration.md) D5). Turning a plugin off withdraws its sources with its row |
| Rpc | Unchanged; it dispatches the new namespace like any owner's |
| Timeline, Dispatcher, rutis, loader | Unchanged |

Public additions: the `settings` service (with `register`), the `settings` management namespace
(two methods), and the JSON shapes of the two existing events. Each one is needed
for a non-Rust owner to read, write and be told about its source, and for a client to edit it.

### 8.1 Decision: keep `permission.rules`/`permission.set_rules`

F2 left open whether to fold its methods into `settings.read/update`. They stay:

- `permission` is a Rust source. Its validation (no separator characters, at least one word) is
  Rust code. `settings.update` validates against a declared JSON Schema, so serving Rust sources
  would need a type-erased registry of Rust documents in Settings. Nothing else needs that.
- `permission.set_rules` already has a typed contract and error codes, and F6 is designed against
  it.
- The rule is the same for MCP and Skills: a Rust owner keeps its own methods, and
  `settings.*` serves declared sources.

## 9. Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (one event bus) | Notification stays on the rutis event bus with S8a's events. No `subscribe`, no watch channel, no second bus. TypeScript parts listen to the same events on the same bus |
| Principles §3 (dependency direction) | Settings depends on no plugin. Owners depend on Settings. No core part depends on an optional plugin |
| §4 rule 1 (command events) | Not used. Settings is process configuration written through its owner's typed write (overview §6.2 rule 2) |
| §4 rule 2 (handover) | Not applicable; no handover between modules |
| §4 rule 3 (before callback) | Not used |
| §4 rule 4 (core has no business meaning) | Settings validates against declared schemas and names no owner |
| §4 rule 5 (batches) | No Timeline writes |
| §4 rule 6 (context) | Nothing enters model context. A tool's view of settings is its owner's business |
| §4 rule 7 (public interfaces only) | Owners use the `settings` service; clients use management methods |
| §4 rule 8 (infrastructure unchanged) | No change to Timeline, Dispatcher, event envelope, rutis, loader, Agent assembly or Session facade. Existing `HostDispatch`, `ServiceCatalog`, `managementMethods` and rutis events are used as they are |
| §4 rule 9 (only needed facts in Timeline) | No event is added; settings are not Timeline facts |
| ADR 002 D1 (scope) | Settings is a process plugin; the sources outlive Sessions |
| ADR 002 D2 (no wiring modules) | No new plugin. The service and methods live in Settings, the owner of the data |
| ADR 002 D3 (dependencies) | No new optional dependency |
| ADR 002 D4 (configuration) | Business configuration in the owner's named source, read at use time, changed with notification, no reload |
| ADR 002 D5 (language-neutral) | §4.1–§4.4 are the contract; §4.5 is the Rust binding |

Requirements raised to mechanism owners: none. TypeScript parts listen on the rutis bus (§10
question 2); that is no new requirement on dim runtime or rutis.

## 10. Open questions

1. ~~**Which manifest carries the `settings` entry.**~~ Settled by ADR 007 D5: no manifest
   carries it; the plugin's code registers its sources (§3.1).
2. ~~**How a TypeScript part listens to rutis events.**~~ Settled: a part listens to the Settings
   channels on rutis's one bus (§4.4). The Session broadcast does not carry them.
3. ~~**Packages installed while running.**~~ Settled: sources are registered at run time by the
   plugin itself (§3.1), so installing, replacing or removing a plugin does not reload Settings.
4. ~~**Which plugin provides `credentials` in the product.**~~ Withdrawn with the service: each
   owner handles its own secrets (§1.2).
5. **Source discovery for a settings UI.** A generic `/settings` page would need the declared
   sources and their schemas (for example a `settings.sources` method). Nothing requires it for
   S8b; it is decided with the CLI settings design.
6. **User-only fields for Rust sources.** No Rust owner writes its source on behalf of a tool
   today. If one does (Scheduler, Workflow), it needs a user-only declaration on its
   `BusinessDocument`. That is decided with that owner's design.

## 11. Acceptance (flow tests)

App Server flow tests (`crates/dim-app-server/tests/settings_flow.rs` and a TypeScript row loaded
through C4's path):

1. A TypeScript test package registers source `example` (user file, project file, schema with a
   `default`, one user-only pointer). Its first `read` returns the default document with revision
   `missing`. An `update` with that revision stores the document and returns a new revision. A
   stale revision fails with `conflict`. A schema violation fails and writes nothing.
2. A client `settings.update` changes the user-only field. The owner's next `update` that changes
   it fails with `user_only_field`; one that keeps it succeeds.
3. A Rust listener on `settings_changes("example")` receives `{source, scope, revision}` after
   each successful write on both paths, and none after a rejected one. The payload serializes to
   the §4.4 JSON shape.
4. Native profile: the user scope persists in row `user/example`; existing v13 rows are unchanged.
   Project scope on an unauthorized root fails with `invalid_scope`.
5. `settings.read` of `mcp` returns `settings_unknown_source`. A row registering `mcp`, a second
   registration of one source, or a malformed declaration fails the registering row. A source
   appears when its owner registers it and is `unknown_source` once the owner unloads, while its
   document stays in the home.
6. ~~Credential references through a shared `credentials` store.~~ Withdrawn (§1.2).
7. A paired-device connection is not served `settings.read`.

## 12. Implementation status

**Part 1 (Rust side, S8b-1).** Implemented without C4: serializable event payloads (§4.4), declared
sources with schema validation, defaults and user-only fields (§3.1–§3.4), the `settings` service
(§4.2) and the `settings.*` methods (§4.3). Declarations came from an embedder list until part 2
moved them onto C4 rows. Part 1 also shipped the `credentials` service and the `*Ref` rule; both
are removed and Provider's own secret host restored (§1.2).

Corrections made against the code (reflected in §4.3 and §4.5):

- The client methods are owned by a separate process part, `SettingsMethods`, not by Settings
  (methods contract §3.2; ADR 004 D4 boundary). This is the only new plugin row; the §2 diagram's
  `managementMethods settings` sits in that part, and §9's ADR 002 D2 answer holds for it as for
  PermissionRules: it is the client path of the Settings data, not a wiring module.
- `SettingsError` has `InvalidScope` and `Document(message)` besides the two named variants, so
  the client path can tell invalid params from a persisted failure.
- The service reply envelope is the one of `sessions`.

Acceptance (§11) covered by Rust flow tests (`settings_flow.rs`, `provider_flow.rs`), with the
owner calling the `settings` service through its `HostDispatch`: items 1–5 and 7.

**Part 2 (S8b-2).** Implemented on C4's path:

- The TypeScript owner path through C4's rows. (Declarations were first the rows' `settings`
  member; part 4 replaced it with registration.)
- `transport_flow/settings_methods.rs` runs a TypeScript owner (`runtime/fixtures/settings-owner.*`)
  in the Node runtime. Item 1 (default document, CAS, stale revision, invalid documents), item 2
  (user-only field: client may, owner may not) and item 3 (a Rust listener sees one §4.4 change per
  successful write on either path, none for a rejected one) pass through its Session methods, which
  call `settings` from TypeScript. The client path (§4.3, item 7) and the registration boundary
  (item 5: a Rust source name, a second registration of one source, a malformed declaration)
  run in the same file. These are `node_` flows, run by CI's Node step.

**Part 3 (S8b-3).** TypeScript parts listen to `settings/<source>` and `settings/projects`
(§4.4, §10 question 2). `transport_flow/settings_methods.rs` checks that the owner fixture's
`ctx.on` listeners receive the three `example` changes of item 3 in commit order, and the
authorization of a project created after the part loaded.

**Part 4 (PluginManager M1, R1).** Sources are registered at run time (§3.1, §4.2, §4.5): the
rows' `settings` member, `SettingsPlugin::reserve`/`declare` and Rust names claimed on first use
are removed. The Rust owners and the TypeScript plugins under `runtime/plugins` register their
sources in their process rows. `settings_flow.rs` checks that a source appears on registration
and is `unknown_source` after its owner unloads while its document stays, and that invalid or
conflicting registrations fail the registering plugin.

Nothing remains open for S8b.
