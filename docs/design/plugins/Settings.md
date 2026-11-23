# Settings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017).

Settings owns the process's configuration documents and the process's authorized projects. This document is the owner's target contract. It replaces the public face described in [Settings service](../appserver/settings-service.md) §4.2–§4.5; the feature's persistence profiles, schema validation, defaults, user-only and `writeOnly` rules, revision semantics and events stay as that document describes. Each source's meaning belongs to the plugin that registers it; those owners have their own contracts ([PromptSettings](PromptSettings.md), [ExecutionSettings](ExecutionSettings.md), [PermissionRules](PermissionRules.md), and so on).

## Use and scope: responsibility split

Settings owns:

- **Sources.** The registry of named documents: each source's file names, project scope, and how its content is checked — a JSON Schema or a Rust document type. A registration belongs to the registering row and ends when that row unloads.
- **Documents and revisions.** The user document (a file, or a row in the native store) and the project document (a file under an authorized project); the opaque revision that changes on every committed write; one writer at a time per document.
- **Change application.** Running a caller's change against the current document under the owner's exclusion, checking the result, writing it, publishing the fact, and reporting whether durability is confirmed.
- **Authority.** An owner change and an agent change may not alter a user-only member; a user edit may. Members marked `writeOnly` are never returned to the agent, and the agent may not set them.
- **Authorized projects.** The cwd → project table, the directory opened for each authorized project, and the fact announcing a new one.
- **Facts.** `settings/<source>` with `{source, scope, revision}` after a commit, and `settings/projects` with `{project, cwd}` after a new authorization.

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
| A user edits a document from a local client | No: the client needs the current revision and a whole-document replace, and a refused write means the user's draft is stale | `read_user`, `replace_user` through [SettingsMethods](SettingsMethods.md) |
| A user's write is based on a revision | Yes: the client holds the revision it read; that revision is the client's business concurrency control between the user's read and write, not this owner's version — the owner's exclusion and revision stay inside `change` | Business; `read_user` returns the draft base and `replace_user` compares it |
| A user edits a Rust-typed source whose rules JSON Schema cannot express | Yes: the source owner's own management namespace performs the write | Business (the source owner's methods) |
| The agent reads one plugin's configuration | No: hiding `writeOnly` members is this owner's schema rule | `agent_read` |
| The agent sets one member of another plugin's configuration | No: refusing user-only and `writeOnly` members is this owner's rule, and a whole-document replace would overwrite members the agent did not read | `agent_set` |
| The plugin tool lists the sources a live row registered | No: the registry is this owner's state | `declared_sources` |
| A process part admits a cwd as a project root | No: the project table and the opened directory are this owner's state | `authorize_project` |
| A business learns the authorized projects | No: the table is this owner's state | `authorized_projects` |
| A business needs a project's cwd or id | Yes: `authorized_projects` is a value with `{project, cwd}` | Business |
| A business reads or writes a project's non-Settings files | Yes: compose the project's cwd with FileStorage's path operations | Business |
| A capability reads a text, switch or policy another part maintains | Yes, but not from Settings: the value's owner provides its own read | The value owner's contract |

## Caller example

ExecutionSettings declares its document in `apply` and keeps the source id for the life of its row.

```rust
let execution = settings
    .register(Register {
        source: "execution".into(),
        user_file: "execution.json".into(),
        project_file: None,
        schema: execution_schema(),
    })?;

// The value as stored now.
let snapshot = settings
    .read(Read { id: execution.clone(), scope: Scope::User }, cancelled.clone())
    .await?;

// A change sees the current document and returns the next one. Settings holds the document for
// the whole call, checks the result and writes it; a competing writer waits.
settings
    .change(Change {
        id: execution.clone(),
        scope: Scope::User,
        change: Arc::new(|current: Option<Document>| {
            Box::pin(async move {
                let mut document = current.map(|document| document.0).unwrap_or_default();
                document.insert("network".into(), true.into());
                Ok(Document(document))
            })
        }),
    }, cancelled.clone())
    .await?;
```

A local client's edit is [SettingsMethods](SettingsMethods.md)'s caller example: the client keeps the draft revision it read, and only that path carries a revision.

The plugin tool reads a plugin's configuration for the agent, and sets one member.

```rust
let view = settings
    .agent_read(AgentRead { source: "web".into() }, cancelled.clone())
    .await?;
// `view.write_only` lists the members withheld from the agent.
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

- [FileStorage](FileStorage.md): `add_root` for an authorized project, `read` and `update` for a document stored as a file, and `create_directory` for the project's directory under `.dimcode`. Settings passes no path outside an authorized root.
- The native store ([#2018](https://github.com/arcships/dim/issues/2018)): read a source's user document, import a file's content when the row is absent, and apply a change under the row's exclusion. The requirement is stated in [Requirements raised to mechanism owners](#requirements-raised-to-mechanism-owners).
- The caller's row: a source id and the registration are bound to the row that registered them; an id from another row is `invalid`.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Make a document addressable under the calling row, checked once | `{source, userFile, projectFile?, schema}` | `{id}` | None; a refusal fails the row's `apply` | `invalid` |
| `read` | The document a source id names, as stored now | `{id, scope}` | `{document}` | While the document is read | `invalid`, `invalid_document`, `too_large`, `closed`, `cancelled` |
| `change` | Apply one change to a document under the owner's exclusion | `{id, scope, change}` | `{}` | Before the write; the change's future is dropped | `invalid`, `invalid_document`, `user_only`, `rejected`, `too_large`, `unsynced`, `closed`, `cancelled` |
| `read_user` | The user's draft base for a document | `{source}` | `{revision, document}` | While the document is read | `unknown_source`, `invalid_document`, `too_large`, `closed`, `cancelled` |
| `replace_user` | Replace a document with the draft the user edited at the revision read | `{source, expectedRevision, document}` | `{revision}` | Before the write | `unknown_source`, `invalid_document`, `conflict`, `too_large`, `unsynced`, `closed`, `cancelled` |
| `agent_read` | A document for the agent, user-only secrets withheld | `{source}` | `{document, writeOnly}` | While the document is read | `unknown_source`, `invalid_document`, `too_large`, `closed`, `cancelled` |
| `agent_set` | Set one member of a document on the agent's behalf | `{source, member, value}` | `{document, writeOnly}` | Before the write | `unknown_source`, `invalid`, `user_only`, `invalid_document`, `too_large`, `unsynced`, `closed`, `cancelled` |
| `declared_sources` | The declarations a live row registered | `{instance}` | `{sources}` | None; the owner completes the read | None |
| `authorize_project` | Admit a cwd as a project root; the same cwd answers the same project | `{cwd}` | `{project}` | While FileStorage opens the directory | `invalid`, `closed`, `cancelled` |
| `authorized_projects` | The projects authorized in this process | `{}` | `{projects: [{project, cwd}]}` | None | `closed` |

Data:

- `SourceId` is the id `register` assigns; it is bound to the row that registered it, and a caller from another row is `invalid`.
- `Document` is one JSON object; a missing user document reads as its default document.
- `Scope` is `"user"` or `{"project": ProjectId}`. `ProjectId` is this process's project id; a project id from another process is not accepted.
- `ChangeFn` is `Fn(Option<Document>) -> Result<Document, Rejected>`: it is called once, under the document's exclusion, with the current document, and returns the next document or its own refusal. It must only compute; waiting on another owner or on I/O inside the change holds the document for that whole wait.
- `Revision` is an opaque value that changes on every committed write. Only the user path carries it, as the draft base the client holds: a business concurrency control between the user's read and write. An owner change carries no revision; the owner's exclusion and any waiting stay inside `change`.
- `writeOnly` lists the JSON Pointers of members withheld from the agent and holding a value.

### Facts

| Fact | Channel | Payload |
| --- | --- | --- |
| A document was written | `settings/<source>` | `{source, scope, revision}` |
| A project was authorized | `settings/projects` | `{project, cwd}` |

A fact is published after the commit and never carries a document. A failed fact delivery is recorded and does not undo the write.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a declaration whose name or file is taken or whose schema or default document is refused, a source id from another row, an unknown project, a project scope for a source without a project file, a member pointer that names no member |
| `unknown_source` | The source is not registered now: retry after its owner registers it, or report that the feature is off |
| `invalid_document` | The document as stored, or the one the change produced, fails its check; repair the user's document or fix the change |
| `user_only` | The change or the agent named a member only the user changes; ask the user, or leave the member as it is |
| `rejected` | The change refused with its own `code` and `message`; act on that code |
| `conflict` | The user's draft is stale; read again and let the user decide |
| `too_large` | The document exceeds the document limit; the caller must not treat the value as a document |
| `unsynced` | The document is in place but its durability is unconfirmed; a read sees the write, and a repeated change confirms it |
| `closed` | This generation is unloading; call the replacement generation |
| `cancelled` | Nothing changed: the read stopped, or the change stopped before the write |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `change` | Take the document's exclusion, read the current document, call the change, check the result, write it, publish, release | Before the write: the change's future is dropped, the document is unchanged, the exclusion is released, `cancelled` | The row's `apply` future ends; the same release applies | New calls are refused; an in-flight change finishes or ends `closed`; the generation releases its exclusions | Exclusion by Settings; the change's future by the calling row |
| `replace_user` | Take the exclusion, read the current revision, refuse a stale base, check and write the document, publish, release | Before the write: nothing changed, `cancelled` | The call ends `cancelled` | In-flight calls finish or end `closed` | Exclusion by Settings |
| `read`, `agent_read` | Open the document through its profile, check it, return the value | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | File handle or row read by Settings or its store |
| `authorize_project` | Open the directory through FileStorage's `add_root`, append to the project table, publish the fact | Before the directory is opened or the row is appended | The call ends `cancelled`; a later call for the same cwd completes the authorization | New calls are refused; the table survives for the next generation, so a project id stays valid | Directory by FileStorage; the project table by Settings |
| `register` | Check the declaration, insert under the calling row, return the id | Not cancellable; a failed `apply` ends the row | The registration is withdrawn with the row; its documents stay | The new generation starts with no registrations; the documents stay | Registration lifetime by the calling row |

## Exceptions with reasons

- The user path carries a draft revision and the owner path does not: the client's business decides whether a write still applies to the document the user read, while an owner change always applies to the current document; the revision is not a version the owner checks on the owner path.
- A document's revision is a value for consumers of the fact, not a write parameter on the owner path: two owners that must agree on a version read the fact and compare revisions.
- `unsynced` reports a write that is in place with unconfirmed durability: a later read sees it, and a repeated change is idempotent, so the caller must not assume the first call failed.
- On the native profile the first read imports the user file into the row when the row is absent: the import is internal state migration, not an operation, and the file stays where it is.
- A project id stays valid across a reload because FileStorage's root table is append-only ([FileStorage](FileStorage.md)); the id is a value, not a handle and not an authority.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Settings is one process part. It publishes facts and folds no state, so it is not a Capability. |
| §2 scopes | Process: documents and the project table outlive Sessions. Session and branch parts read values or follow facts; none registers a source. |
| §3 orthogonality | Settings reads no other owner's state except through FileStorage's operations and the native store's operations; no plugin's business appears in this contract. |
| §4 rule 1 | A committed write is a fact on `settings/<source>`; a refused write publishes nothing. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No `before` callback. `change` runs under the document's exclusion, never inside the Timeline's commit lock. |
| §4 rule 4 | No document meaning, schema or business rule for a source in the core; each source owner keeps its own. |
| §4 rule 5 | No Timeline batch. Settings documents never enter the Timeline. |
| §4 rule 6 | Settings documents never enter model context; the agent path is the `plugin` tool's result, decided by PluginManager. |
| §4 rule 7 | Callers use these operations; no caller reads a file path, a lock or a stage. |
| §4 rule 8 | Supported by the native store's operations ([#2018](https://github.com/arcships/dim/issues/2018)); stated below. |
| §4 rule 9 | A revision fact is written only after the commit; no scratch state is persisted. |
| §7 objects | Only values cross: documents, source ids, project ids, revisions and the `change` callback by reference. No object, handle or path crosses. |
| §7 no proxy | No operation answers another owner's data: a value is requested from the part that owns it, and workflow definition bytes come from FileStorage for the Workflow plugin. |
| §7 one face | One service on the `Settings` type; no facade, no wrapper type, no second name. |
| §7 cross-language | Every operation's request, reply and errors are data; the Rust binding is the appendix. |
| §7 caller complexity | The revision, exclusion, retry and durability stay inside Settings; the caller handles `conflict`, `user_only` and `rejected` only. |
| §7 one change | The old `settings`/`settingsService` surfaces and their callers are migrated in one change; PluginManager, SettingsMethods and every source owner appear in the delivery table. |
| ADR 002 D1 | Process scope; documents and the project table outlive Sessions. |
| ADR 002 D2 | No wiring plugin; the service is on the owner type. |
| ADR 002 D3 | Dependencies point to FileStorage and the native store; no plugin depends on Settings' internals. |
| ADR 002 D4 | Business configuration lives in the owner's named source, read at use time; structural items stay in PluginManager. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| §7.2 checks | Every caller need lands on one operation; no handle, object or proxy crosses; one face; no Rust-only method on the face; no caller mechanism; one change. |

## Requirements raised to mechanism owners

- **SessionStore / native store, [#2018](https://github.com/arcships/dim/issues/2018):** the native user-document operations Settings needs — read one source's document, import a file's content when the row is absent, and apply a change under the row's exclusion, returning the committed document and entering the revision. No object or transaction crosses.
- **Provider, [#2019](https://github.com/arcships/dim/issues/2019):** the model request's User-Agent is read from [ProviderSettings](ProviderSettings.md)'s value instead of being pushed into the process identity; the model-log recorder reads its destination from [ModelLog](ModelLog.md)'s value instead of a process-global setter.
- **Workflow, [#2033](https://github.com/arcships/dim/issues/2033):** definition files are read and replaced through FileStorage's path operations, with the project resolved from `authorized_projects`; `workflowFiles` is removed with no replacement face on Settings.

## Binding appendix

The service is marked on the owner type; the wire name stays `settings`. The `Settings` facade and the `SettingsService` type merge into this type.

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use serde_json::Value;
use std::sync::Arc;

pub type SourceId = String;
pub type ProjectId = String;
pub type Revision = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registered {
    pub id: SourceId,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Document(pub Value);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    User,
    Project { project: ProjectId },
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Rejected {
    pub code: String,
    pub message: String,
}

pub type ChangeFn =
    Arc<dyn Fn(Option<Document>) -> BoxFuture<'static, Result<Document, Rejected>> + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("the settings source is not registered: {0}")]
    UnknownSource(String),
    #[error("the settings document is invalid: {0}")]
    InvalidDocument(String),
    #[error("the member {0} is changed only by the user")]
    UserOnly(String),
    #[error("{code}: {message}")]
    Rejected { code: String, message: String },
    #[error("the settings revision is stale")]
    Conflict,
    #[error("the settings document is {size} bytes, over the document limit")]
    TooLarge { size: u64 },
    #[error("the settings document is in place but its durability is unconfirmed")]
    Unsynced,
    #[error("the settings service is unloading")]
    Closed,
    #[error("the settings call was cancelled")]
    Cancelled,
}

impl PublicService for Settings {}

#[rutis::service(name = "settings")]
impl Settings {
    pub fn register(&self, request: Register) -> Result<Registered, SettingsError> { /* ... */ }
    pub async fn read(&self, request: Read, cancelled: CancelledFn) -> Result<Snapshot, SettingsError> { /* ... */ }
    pub async fn change(&self, request: Change, cancelled: CancelledFn) -> Result<(), SettingsError> { /* ... */ }
    pub async fn read_user(&self, request: ReadUser, cancelled: CancelledFn) -> Result<UserSnapshot, SettingsError> { /* ... */ }
    pub async fn replace_user(
        &self,
        request: ReplaceUser,
        cancelled: CancelledFn,
    ) -> Result<UserSnapshot, SettingsError> { /* ... */ }
    pub async fn agent_read(&self, request: ReadUser, cancelled: CancelledFn) -> Result<AgentSnapshot, SettingsError> { /* ... */ }
    pub async fn agent_set(
        &self,
        request: SetMember,
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
/// its codec instead of a schema.
impl Settings {
    pub fn register_typed<D: BusinessDocument>(&self) -> Result<SourceId, SettingsError> { /* ... */ }
}
```
