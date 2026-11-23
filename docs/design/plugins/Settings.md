# Settings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017).

Settings owns the process's configuration documents and the process's authorized projects. This document is the owner's target contract. Each source's meaning belongs to the plugin that registers it; those owners have their own contracts ([PromptSettings](PromptSettings.md), [ExecutionSettings](ExecutionSettings.md), [PermissionRules](PermissionRules.md), and so on).

## Use and scope: responsibility split

Settings owns:

- **Sources.** The registry of named documents: each source's file names, project scope, the members only the user changes, and how its content is checked — a JSON Schema or a Rust document type. A registration belongs to the registering row and ends when that row unloads.
- **Documents and revisions.** The user document (a file, or a row in the native store) and the project document (a file under an authorized project); the opaque revision that changes on every committed write; one writer at a time per document.
- **Change application.** Running a caller's change against the current document under the owner's exclusion, checking the result, writing it, publishing the fact, and reporting whether durability is confirmed.
- **Authority.** An owner change and an agent change may not alter a user-only member; a user edit may. Members marked `writeOnly` are never returned to the agent, and the agent may not set them.
- **Authorized projects.** The cwd → project table, the directory opened for each authorized project, and the fact announcing a new one.
- **Facts.** `settings/<source>` with `{source, scope, revision}` after a commit, and `settings/projects` with `{root, cwd}` after a new authorization.

Callers keep:

- Document meaning: the type, schema, defaults and validation rules of each source, and the business name of every member.
- Which source, member or project a business edits, and what a refused or conflicting edit means for that business.
- When a value is fresh enough: a reader reads at use time, or follows the fact and reads again.
- Permission and approval decisions; Settings never decides whether a write is allowed beyond the authority rules above.
- File bytes of non-Settings files, including workflow definitions: those are FileStorage's operations, requested by the file's owner.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A source owner reads its own document now | No: addressing by source name lets every row read every source, and makes the owner re-resolve its registration on each call | `register` answers the source id; `read` takes the id |
| A source owner changes its own document | No: a read-then-replace makes the caller hold the revision and retry a competing write — the owner's exclusion and revision are its own mechanism | `change` |
| A source owner reacts to a committed write | Yes: the `settings/<source>` fact carries the revision; read again | Business |
| A user edits a declared document from a client | No: the client needs the current revision and a whole-document replace, and a refused write means the user's draft is stale | `read_user`, `replace_user` through [SettingsMethods](SettingsMethods.md), which resolves the source name and refuses a Rust source |
| A user's write is based on a revision | Yes: the client holds the revision it read; that revision is the client's business concurrency control between the user's read and write, not this owner's version — the owner's exclusion and revision stay inside `change` | Business; `read_user` returns the draft base and `replace_user` compares it |
| A user edits a Rust-typed source whose rules JSON Schema cannot express | Yes: the source owner's own management namespace performs the write with its source id | Business (the source owner's methods) |
| The agent reads one plugin's configuration | No: hiding `writeOnly` members is this owner's schema rule | `agent_read` |
| The agent sets one member of another plugin's configuration | No: refusing user-only and `writeOnly` members is this owner's rule, and a whole-document replace would overwrite members the agent did not read | `agent_set` |
| The plugin tool lists the sources a live row registered | No: the registry is this owner's state | `declared_sources` |
| A process part admits a cwd as a project root | No: the project table and the opened directory are this owner's state | `authorize_project` |
| A business learns the authorized projects | No: the table is this owner's state | `authorized_projects` |
| A business needs a project's cwd or number | Yes: `authorized_projects` is a value with `{root, cwd}` | Business |
| A business reads or writes a project's non-Settings files | Yes: compose the project's cwd with FileStorage's path operations | Business |
| A capability reads a text, switch or policy another part maintains | Yes, but not from Settings: the value's owner provides its own read | The value owner's contract |

## Caller example

ExecutionSettings declares its document in `apply` and keeps the source id for the life of its row.

```rust
let execution: SourceId = settings.register(
    ctx,
    Register {
        source: "execution".into(),
        user_file: "execution.json".into(),
        project_file: None,
        schema: execution_schema(),
        user_only: vec!["/sandbox".into(), "/network".into()],
    },
)?;

// The value as stored now.
let snapshot = settings
    .read(ctx, Read { id: execution.clone(), scope: Scope::User }, cancelled.clone())
    .await?;

// A change sees the current document and returns the next one. Settings holds the document for
// the whole call, checks the result and writes it; a competing writer waits.
settings
    .change(
        ctx,
        execution.clone(),
        Scope::User,
        Arc::new(|current: Option<Document>| {
            Box::pin(async move {
                let mut document = current.map(|document| document.0).unwrap_or_default();
                document["network"] = true.into();
                Ok(Document(document))
            })
        }),
        cancelled.clone(),
    )
    .await?;
```

A user edit is [SettingsMethods](SettingsMethods.md)'s caller example: the client names a declared source, keeps the draft revision it read, and only that path carries a revision.

The plugin tool reads a plugin's configuration for the agent, and sets one member.

```rust
let view = settings
    .agent_read(AgentRead { source: "web".into() }, cancelled.clone())
    .await?;
// `view.writeOnly` lists the members withheld from the agent.
settings
    .agent_set(
        AgentSet { source: "web".into(), member: "/proxyDns".into(), value: true.into() },
        cancelled.clone(),
    )
    .await?;
```

Workflow resolves its project and touches its own definition files through FileStorage.

```rust
let projects = settings.authorized_projects();
let project = projects
    .projects
    .iter()
    .find(|project| project.cwd == cwd)
    .ok_or(WorkflowError::WorkspaceNotAuthorized)?;
let definitions = format!("{}/.dimcode/workflows/{}", project.cwd, name);
let bytes = storage.read(definitions, cancelled).await?; // FileStorage's operation
```

## Own contract

### Dependencies

- [FileStorage](https://github.com/arcships/dim/pull/2080): `add_root` for an authorized project, `read` and `update` for a document stored as a file, and `create_directory` for the project's directory under `.dimcode`. Settings passes no path outside an authorized root.
- The storage engine ([#2018](https://github.com/arcships/dim/issues/2018)) as the native profile's crate-private implementation: it executes this owner's transactions over the `settings_documents` rows. The rows and their request, reply and error shapes are this contract's ([Native-profile row operations](#native-profile-row-operations)); the engine answers no Settings read or write on its public face.
- The caller's row: a source id and the registration are bound to the row that registered them; an id from another row is `invalid`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Make a document addressable under the calling row, checked once | `{source, userFile, projectFile?, schema, userOnly}` | a source id | None; a refusal fails the row's `apply` | `invalid` |
| `read` | The document a source id names, as stored now | `{id, scope}` | `{document}` | While the document is read | `invalid`, `invalid_document`, `too_large`, `failed`, `closed`, `cancelled` |
| `change` | Apply one change to a document under the owner's exclusion | `{id, scope}` and the change callback | `{}` | Before the write; the change's future is dropped | `invalid`, `invalid_document`, `user_only`, `rejected`, `too_large`, `unsynced`, `failed`, `closed`, `cancelled` |
| `read_user` | The user's draft base for a document, by source id | `{id, scope}` | `{revision, document}` | While the document is read | `invalid`, `invalid_document`, `too_large`, `failed`, `closed`, `cancelled` |
| `replace_user` | Replace a document with the draft the user edited at the revision read | `{id, scope, expectedRevision, document}` | `{revision, document}` | Before the write; an admitted write still commits | `invalid`, `invalid_document`, `conflict`, `too_large`, `unsynced`, `failed`, `closed`, `cancelled` |
| `agent_read` | A declared document for the agent, `writeOnly` members withheld | `{source}` | `{document, writeOnly}` | While the document is read | `unknown_source`, `invalid_document`, `too_large`, `failed`, `closed`, `cancelled` |
| `agent_set` | Set one member of a declared document for the agent | `{source, member, value}` | `{document, writeOnly}` | Before the write | `unknown_source`, `invalid`, `user_only`, `invalid_document`, `too_large`, `unsynced`, `failed`, `closed`, `cancelled` |
| `declared_sources` | The declared sources, by name: all of them, or those a live row registered | `{instance?}` | `{sources: [{id, source, schema, scopes, userOnly, writeOnly}]}` | None; the owner completes the read | None |
| `authorize_project` | Admit a cwd as a project root; the same cwd answers the same number | `{cwd}` | `{root}` | While FileStorage opens the directory | `invalid`, `failed`, `closed`, `cancelled` |
| `authorized_projects` | The projects authorized in this process generation | `{}` | `{projects: [{root, cwd}]}` | None | None |

Data:

- `SourceId` is the id `register` assigns; it is bound to the row that registered it, and a caller from another row is `invalid`. `declared_sources` lists only declared sources; a Rust source's id is not listed.
- `Document` is one JSON object; a missing user document reads as its default document.
- `Scope` is `"user"` or `{"project": <number>}`. The number is this process's project number, assigned by `authorize_project`; it is not durable and it is not FileStorage's root id.
- `userOnly` lists JSON Pointers to members only the user changes. Each pointer must name a member reachable through the schema's `properties`; otherwise `register` is `invalid`.
- `ChangeFn` is `Fn(Option<Document>) -> Result<Document, ChangeRefused>`: it is called once, under the document's exclusion, with the current document, and returns the next document or its own refusal. It must only compute; waiting on another owner or on I/O inside the change holds the document for that whole wait.
- `Revision` is an opaque value that changes on every committed write. Only the user path carries it, as the draft base the client holds: a business concurrency control between the user's read and write. An owner change carries no revision; the owner's exclusion and any waiting stay inside `change`.
- `writeOnly` lists the JSON Pointers of members withheld from the agent and holding a value.

### Native-profile row operations

The native profile stores each user document as a `settings_documents` row. These four operations belong to this contract and are crate-private to the storage crate: the engine executes each as one transaction and returns the stored form, and no caller outside Settings calls them. The file profile has no counterpart; it calls FileStorage's `read` and `update` instead.

| Operation | Intent | Request | Reply | Errors |
| --- | --- | --- | --- | --- |
| `readUser` | One source's stored user document | `{source}` | `{version, document}` or none | `failed` |
| `importUserIfAbsent` | Import the validated file content when the row is absent, in one transaction, and return the winning row | `{source, document, importedRevision}` | `{version, document}` | `failed` |
| `changeUser` | Apply one change under the row's exclusion | `{source}` and the change callback | `{version, document}` | `rejected`, `failed` |
| `replaceUser` | Replace a user document at an expected version | `{source, expectedVersion, document}` | `{version}` | `conflict`, `missing`, `overflow`, `failed` |

`change` in the public operations is `changeUser` on the native profile and FileStorage's `update` on the file profile; `replace_user` is `replaceUser` and FileStorage's `update`. On the file profile, Settings reads the current revision inside the FileStorage `update` callback and compares `expectedRevision` there; FileStorage holds only the file key and knows no version. `importUserIfAbsent` runs on the first read of a source whose file exists and whose row does not; a version is a positive integer that increases on every committed write, and the revision Settings publishes is derived from it.

### Facts

| Fact | Channel | Payload |
| --- | --- | --- |
| A document was written | `settings/<source>` | `{source, scope, revision}` |
| A project was authorized | `settings/projects` | `{root, cwd}` |

A fact is published after the commit and never carries a document. A failed fact delivery is recorded and does not undo the write.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a declaration whose name or file is taken, whose schema is refused, or whose user-only pointer names no reachable member; a source id from another row; an unknown project; a project scope for a source without a project file; a member pointer that names no member |
| `unknown_source` | The declared source is not registered now, or it is a Rust source that the client and agent paths do not serve: retry after its owner registers it, or report that the feature is off |
| `invalid_document` | The document as stored, or the one the change produced, fails its check; repair the user's document or fix the change |
| `user_only` | The change or the agent named a member only the user changes; ask the user, or leave the member as it is |
| `rejected` | The change refused with its own `code` and `message`; act on that code |
| `conflict` | The user's draft is stale; read again and let the user decide |
| `too_large` | The document exceeds the document limit; the caller must not treat the value as a document |
| `unsynced` | The document is in place but its durability is unconfirmed; read it and do not repeat a change without reading |
| `failed` | Storage or persistence failed: a FileStorage read or write, a row transaction, or a worker; read again before retrying |
| `closed` | This generation is unloading; call the replacement generation |
| `cancelled` | Nothing changed: the read stopped, or the change stopped before the write |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `change` | Take the document's exclusion, read the current document, call the change, check the result, write it, publish, release | Before the write: the change's future is dropped, the document is unchanged, the exclusion is released, `cancelled` | The row's `apply` future ends; the same release applies | New calls are refused; an in-flight change finishes or ends `closed`; the generation releases its exclusions | Exclusion by Settings; the change's future by the calling row |
| `replace_user` | Take the exclusion, read the current revision, refuse a stale base, check and write the document, publish, release | An admitted write still commits or ends `unsynced`; a wait dropped before admission changes nothing | The admitted write completes; the reply may be lost | In-flight calls finish or end `closed` | Exclusion by Settings; the admitted write by Settings |
| `read`, `read_user`, `agent_read`, `declared_sources`, `authorized_projects` | Open the document through its profile or read the registry, check it, return the value | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | File handle or row read by Settings |
| `authorize_project` | Open the directory through FileStorage's `add_root`, append to the project table, publish the fact | Before the directory is opened or the row is appended | The call ends `cancelled`; a later call for the same cwd completes the authorization | The table is generation state: a new generation starts empty and a cwd is authorized again at admission | Directory by FileStorage; the project table by Settings |
| `register` | Check the declaration, insert under the calling row, return the id | Not cancellable; a failed `apply` ends the row | The registration is withdrawn with the row; its documents stay | The new generation starts with no registrations; the documents stay | Registration lifetime by the calling row |

## Exceptions with reasons

- The user path carries a draft revision and the owner path does not: the client's business decides whether a write still applies to the document the user read, while an owner change always applies to the current document; the revision is not a version the owner checks on the owner path.
- `unsynced` reports a write that is in place with unconfirmed durability: a read sees it, and a repeated change applies again, so the caller reads before repeating.
- On the native profile the first read imports the user file into the row when the row is absent: the import is internal state migration, not an operation, and the file stays where it is.
- The project table is process-generation state: a project number is not durable and does not survive a Settings reload; a caller that needs a project after a reload resolves its cwd again at admission, which re-authorizes it.
- A client write is admitted when it starts: a disconnected client does not undo it, so the reply may be lost while the document is written or `unsynced`.

## Supersedes

- [Settings service](../appserver/settings-service.md) §4.2 (the `settings` service) and §4.5 (the Rust binding); §4.3 is [SettingsMethods](SettingsMethods.md)'s. The document's §3 state, §4.1 shapes, §4.4 events, §6 errors' state meanings, §7 security and §11 acceptance stay in force; §4.1's `Scope` is the `Scope` below and §4.4's `project_authorized` payload keeps `{root, cwd}`.
- [Settings service](../appserver/settings-service.md) §8.1's note that Rust owners keep their own methods stays; the methods' shapes are in each owner's contract.
- [Resource owner operations](../contracts/resource-owner-operations.md) §Settings.
- [Service faces](../service-faces.md) §3.3's `settingsService` row, §3.2's host-port sentence for the three prompt ports, and §4.1's `settingsService` paragraph.
- [Management methods](../appserver/management-methods.md)'s `settings.read`/`update`/`sources`/`projects` list.
- [Prompt overrides](../appserver/prompt-overrides.md) §3.1 (the three prompt ports are one service; see [PromptSettings](PromptSettings.md)).
- [Permission command rules](../appserver/permission-command-rules.md) §4.1's `commandRules` service (see [PermissionRules](PermissionRules.md)).

## Rule check

Rules: [§7](https://github.com/arcships/dim/blob/2fc4f50b47529e17d51e92b1d7441734771226fc/docs/architecture/rust-agent/principles.md#7-所有者与操作), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Settings is one process part. It publishes facts and folds no state, so it is not a Capability. |
| §2 scopes | Process: documents and the project table outlive Sessions. Session and branch parts read values or follow facts; none registers a source. |
| §3 orthogonality | Settings reads no other owner's state except through FileStorage's operations and the storage engine's transactions; no plugin's business appears in this contract. |
| §4 rule 1 | A committed write is a fact on `settings/<source>`; a refused write publishes nothing. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No `before` callback. `change` runs under the document's exclusion, never inside the Timeline's commit lock. |
| §4 rule 4 | No document meaning, schema or business rule for a source in the core; each source owner keeps its own. |
| §4 rule 5 | No Timeline batch. Settings documents never enter the Timeline. |
| §4 rule 6 | Settings documents never enter model context; the agent path is the `plugin` tool's result, decided by PluginManager. |
| §4 rule 7 | Callers use these operations; no caller reads a file path, a lock or a stage. |
| §4 rule 8 | The storage engine executes this owner's crate-private row operations ([#2091](https://github.com/arcships/dim/pull/2091)); no new infrastructure. |
| §4 rule 9 | A revision fact is written only after the commit; no scratch state is persisted. |
| §7 objects | Only values cross: documents, source ids, project numbers, revisions and the change callback by reference. No object, handle or path crosses. |
| §7 no proxy | No operation answers another owner's data: a value is requested from the part that owns it, and workflow definition bytes come from FileStorage for the Workflow plugin. |
| §7 one face | One service on the `Settings` type; no facade, no wrapper type, no second name. |
| §7 cross-language | Every operation's request, reply and errors are data; the Rust binding is the appendix. |
| §7 caller complexity | The revision, exclusion, retry and durability stay inside Settings; the caller handles `conflict`, `user_only`, `rejected` and `unsynced` only. |
| §7 one change | The old `settings`/`settingsService` surfaces and their callers are migrated in one change; PluginManager, SettingsMethods and every source owner appear in the delivery table. |
| ADR 002 D1 | Process scope; documents and the project table outlive Sessions. |
| ADR 002 D2 | No wiring plugin; the service is on the owner type. |
| ADR 002 D3 | Dependencies point to FileStorage and the storage engine; the rows belong to Settings and no plugin depends on Settings' internals. |
| ADR 002 D4 | Business configuration lives in the owner's named source, read at use time; structural items stay in PluginManager. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| ADR 027 | Not applicable: every caller is at the process scope or depends outward, so no cross-scope id resolution is involved. |
| §7.2 checks | Every caller need lands on one operation; no handle, object or proxy crosses; one face; no Rust-only method on the face; no caller mechanism; one change. |

## Requirements raised to mechanism owners

- **Storage engine, [#2018](https://github.com/arcships/dim/issues/2018) and [#2091](https://github.com/arcships/dim/pull/2091):** no requirement. The engine executes this owner's crate-private row operations as transactions; the rows, versions and conflicts are Settings', and the engine publishes none of them under `sqliteStorage`.
- **Provider, [#2019](https://github.com/arcships/dim/issues/2019):** the model request's User-Agent is read from [ProviderSettings](ProviderSettings.md)'s value instead of being pushed into the process identity; the model-log recorder reads its destination from [ModelLog](ModelLog.md)'s value instead of a process-global setter.
- **Workflow, [#2033](https://github.com/arcships/dim/issues/2033):** definition files are read and replaced through FileStorage's path operations, with the project resolved from `authorized_projects`; `workflowFiles` is removed with no replacement face on Settings.

## Binding appendix

The service is marked on the owner type; the wire name stays `settings`. The `Settings` facade and the `SettingsService` type merge into this type.

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use rutis::Ctx;
use serde_json::Value;
use std::sync::Arc;

pub type SourceId = String;
/// The process-local project number `authorize_project` assigns; it is not durable.
pub type ProjectId = u64;
pub type Revision = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Register {
    pub source: String,
    pub user_file: String,
    pub project_file: Option<String>,
    pub schema: Value,
    pub user_only: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Document(pub Value);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    User,
    Project(ProjectId),
}

impl Scope {
    fn project(&self) -> Option<ProjectId> {
        match self {
            Self::User => None,
            Self::Project(project) => Some(*project),
        }
    }
}

/// One document's replacement from the change callback: its own `code` and `message`.
#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ChangeRefused {
    pub code: String,
    pub message: String,
}

/// A callback's refusal arrives as the wire's own error; its code and message are kept.
impl From<rutis::host::CallError> for ChangeRefused {
    fn from(error: rutis::host::CallError) -> Self {
        let code = error
            .raw()
            .and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>())
            .and_then(rutis_bridge::session::Error::code)
            .unwrap_or("rejected")
            .to_owned();
        Self { code, message: error.to_string() }
    }
}

pub type ChangeFn = Arc<
    dyn Fn(Option<Document>) -> BoxFuture<'static, Result<Document, ChangeRefused>> + Send + Sync,
>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Read {
    pub id: SourceId,
    pub scope: Scope,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub document: Document,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSnapshot {
    pub revision: Revision,
    pub document: Document,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceUser {
    pub id: SourceId,
    pub scope: Scope,
    pub expected_revision: Revision,
    pub document: Document,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRead {
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub document: Document,
    pub write_only: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSet {
    pub source: String,
    pub member: String,
    pub value: Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByInstance {
    pub instance: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Declaration {
    pub id: SourceId,
    pub source: String,
    pub schema: Value,
    pub scopes: Vec<String>,
    pub user_only: Vec<String>,
    pub write_only: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Declarations {
    pub sources: Vec<Declaration>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeProject {
    pub cwd: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Authorized {
    pub root: ProjectId,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub root: ProjectId,
    pub cwd: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedProjects {
    pub projects: Vec<Project>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    UnknownSource(String),
    #[error("{0}")]
    InvalidDocument(String),
    #[error("{0}")]
    UserOnly(String),
    #[error("{code}: {message}")]
    Refused(ChangeRefused),
    #[error("the settings revision is stale")]
    Conflict,
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    Unsynced(String),
    #[error("{0}")]
    Failed(String),
    #[error("the settings service is unloading")]
    Closed,
    #[error("the settings call was cancelled")]
    Cancelled,
}

impl SettingsError {
    /// The wire `code`; a refused change keeps the callback's own code.
    pub fn code(&self) -> &str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::UnknownSource(_) => "unknown_source",
            Self::InvalidDocument(_) => "invalid_document",
            Self::UserOnly(_) => "user_only",
            Self::Refused(refused) => &refused.code,
            Self::Conflict => "conflict",
            Self::TooLarge(_) => "too_large",
            Self::Unsynced(_) => "unsynced",
            Self::Failed(_) => "failed",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The wire error's `{code, message}` shape; derived tags cannot carry the callback's code.
impl serde::Serialize for SettingsError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("SettingsError", 2)?;
        out.serialize_field("code", self.code())?;
        out.serialize_field("message", &self.to_string())?;
        out.end()
    }
}

impl PublicService for Settings {}

#[rutis::service(name = "settings")]
impl Settings {
    pub fn register(&self, caller: &Ctx, request: Register) -> Result<SourceId, SettingsError> { /* ... */ }
    pub async fn read(
        &self,
        caller: &Ctx,
        request: Read,
        cancelled: CancelledFn,
    ) -> Result<Snapshot, SettingsError> { /* ... */ }
    pub async fn change(
        &self,
        caller: &Ctx,
        id: SourceId,
        scope: Scope,
        change: ChangeFn,
        cancelled: CancelledFn,
    ) -> Result<(), SettingsError> { /* ... */ }
    pub async fn read_user(
        &self,
        request: Read,
        cancelled: CancelledFn,
    ) -> Result<UserSnapshot, SettingsError> { /* ... */ }
    pub async fn replace_user(
        &self,
        request: ReplaceUser,
        cancelled: CancelledFn,
    ) -> Result<UserSnapshot, SettingsError> { /* ... */ }
    pub async fn agent_read(
        &self,
        request: AgentRead,
        cancelled: CancelledFn,
    ) -> Result<AgentSnapshot, SettingsError> { /* ... */ }
    pub async fn agent_set(
        &self,
        request: AgentSet,
        cancelled: CancelledFn,
    ) -> Result<AgentSnapshot, SettingsError> { /* ... */ }
    pub fn declared_sources(&self, request: ByInstance) -> Declarations { /* ... */ }
    pub async fn authorize_project(
        &self,
        request: AuthorizeProject,
        cancelled: CancelledFn,
    ) -> Result<Authorized, SettingsError> { /* ... */ }
    pub fn authorized_projects(&self) -> AuthorizedProjects { /* ... */ }
}

// `CancelledFn` is the binding's cancellation argument, not a request field.

/// The Rust binding of `register` for a Rust document type: the same operation, with the type as
/// its codec instead of a schema, and its `USER_ONLY` list as the user-only members.
impl Settings {
    pub fn register_typed<D: BusinessDocument>(
        &self,
        caller: &Ctx,
    ) -> Result<SourceId, SettingsError> { /* ... */ }
}

/// The Rust document binding gains the user-only rule its source declares.
pub trait BusinessDocument {
    // ... `NAMESPACE`, `USER_FILE`, `PROJECT_FILE` as today
    const USER_ONLY: &'static [&'static str] = &[];
}
```
