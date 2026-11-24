# UiExtensions

Scope: process. Issue: #2021.

UiExtensions owns the daemon's UI content catalog: which plugin packages contribute interface panels, what each declared, the prepared generation of each, and the directory clients read. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/ui_extensions/`, and the public contract of the client methods is [client UI extensions](../client-ui-extensions.md). PluginManager, FileStorage, Connection and UiResources are separate owners.

## Use and scope: responsibility split

UiExtensions owns:

- **Registrations.** One entry per plugin id: the declaration, the prepared generation and the refusal reason when preparation failed.
- **Preparation.** Copying the declared content into the owner's cache, validating versions, roots and panel files, and publishing a generation atomically.
- **The directory value.** Revisions, extensions and errors as clients read them.
- **Bounded reads.** A page of a prepared file, by generation and path.
- **The change event.** One process event carrying the revision when the published directory changes.

Callers keep:

- Which package is installed and enabled: PluginManager decides and forwards the plugin id and declaration.
- What a panel renders: the client owns mounting and selection.
- Writing the cache: the cache is UiExtensions' own directory.
- Telling a client: Connection turns the change event into the client's notification.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| PluginManager registers a package's UI declaration by plugin id | No: validation, preparation and the atomic generation switch are the owner's | `register` |
| PluginManager withdraws a package | No: the generation and its reads must be settled | `withdraw` |
| PluginManager lists the packages with content and asks why one was refused | Yes: `plugins` and `refusal` compose | `plugins`, `refusal` |
| A client reads the directory and a panel file | No: the revision and the prepared generation are the owner's | `directory`, `read` |
| A client is told the directory changed | Yes: the change event composes | event |

An operation or parameter exists only where the row says No.

## Caller example

PluginManager registers a package's declaration:

```rust
ui.register(RegisterRequest { plugin: id, manifest: Some(declaration) }).await?;
// A refusal is reported in the directory; the package's code row is unaffected.
```

A client reads the directory and one panel file:

```rust
let directory = ui.directory().await?; // revision, extensions, errors
let page = ui.read(ReadRequest { generation, path, offset: 0, limit: None }).await?;
```

## Own contract

### Dependencies

FileStorage (`add_root`, `list`, `read`, and `open_read`/`read_at`/`close_read` for a page of a prepared file; the reader id stays inside this owner); ManagementMethods' `register` for the `ui.extensions` namespace; PartRegistry to mount the UiResources Session part (defined in the [UiResources contract](UiResources.md)).

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish one package's UI content | `{plugin, manifest?}` | `{}`; a failed preparation is recorded as a refusal, not an error | While the files are copied; nothing is published | `invalid_params` (unknown plugin id, unsupported version, bad declaration) |
| `withdraw` | Remove a package's content | `{plugin}` | `{}` | None; the owner settles admitted reads | none |
| `plugins` | The packages with content now | `{}` | `{plugins: [id]}` | None | none |
| `refusal` | Why a package's content was refused | `{plugin}` | `{message?}` | None | none |
| `directory` | The published directory | `{}` | `{revision, extensions, errors}` | None | none |
| `read` | One page of a prepared file | `{generation, path, offset?, limit?}` | `{offset, bytes, total, media_type, data}` | The calling connection's task ends; the page stops | `resource_unavailable` |

`register` with no manifest withdraws the package's content, the same as `withdraw`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_params` | The package id is unknown, the version unsupported or the declaration invalid; the reason is recorded as the package's refusal |
| `resource_unavailable` | The file is gone or unreadable; the client shows the panel as unavailable |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Validate, copy into a new generation directory, swap the catalog entry, bump the revision, emit the change event | The generation directory is removed, the old entry stays | The registration is dropped with the process row | Sources are retained and restored on the next generation | UiExtensions: the generation directory and its reads |
| `withdraw` | Remove the entry, cancel admitted reads, wait for them, remove the directory, emit the change event | — | — | — | UiExtensions |
| `read` | Resolve the generation and the path, read one bounded page under the read counter | The page stops; nothing changes | The read token is released | Admitted reads are cancelled and awaited before the directory goes | UiExtensions |

The `ui.extensions.read` client method is the same `read`, served to clients; WebEntry's HTTP route is its transport. Both stay.

## Exceptions with reasons

- `register` takes the declaration as an optional value: a package with no UI member withdraws, and the owner resolves the installation directory by plugin id through PluginManager, so no path or handle crosses.
- The change event is the owner's fact, not a subscription: a subscriber reads `directory` when the revision differs, so no freshness state crosses.

## Supersedes

- [Client UI extensions](../client-ui-extensions.md) service table row `uiExtensions`: the operations are those in this contract.
- [Service faces](../service-faces.md) §3.6 row `uiExtensions`.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.


| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | UiExtensions is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: content serves every connection and every Session. |
| §3 orthogonality | No other owner's state is read; PluginManager and FileStorage are asked through operations. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact; the change event is a process event, not a Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the catalog or a prepared file handle. |
| §7 no proxy | The catalog, the generation and the reads are this owner's; FileStorage supplies bytes. |
| ADR 002 D1 | Process scope: content outlives Sessions. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | PluginManager and the client entry depend on this owner; it depends on FileStorage and the method table. |
| ADR 002 D4 | Content declarations are structural (the package manifest); no business setting. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None. FileStorage's contract (#2016) supplies `add_root`, `list`, `read`, `open_read`, `read_at` and `close_read`.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryReply {
    pub revision: u64,
    pub extensions: Vec<Extension>,
    pub errors: Vec<DirectoryError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRequest {
    pub generation: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentPage {
    pub offset: u64,
    pub bytes: usize,
    pub total: u64,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ContentError {
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("The UI content file is unavailable")]
    ResourceUnavailable,
}

#[rutis::service(name = "uiExtensions")]
impl UiExtensions {
    pub async fn register(&self, request: RegisterRequest) -> Result<(), ContentError> { /* ... */ }
    pub async fn withdraw(&self, request: PluginRef) { /* ... */ }
    pub async fn plugins(&self, request: Empty) -> PluginsReply { /* ... */ }
    pub async fn refusal(&self, request: PluginRef) -> RefusalReply { /* ... */ }
    pub async fn directory(&self, request: Empty) -> DirectoryReply { /* ... */ }
    pub async fn read(&self, request: ReadRequest) -> Result<ContentPage, ContentError> { /* ... */ }
}
```

The process event is `uiExtensions/changed {revision}`; Connection carries the revision to clients that asked for it.
