# FileStorage

Scope: process. Issue: #2016.

FileStorage owns byte storage under explicitly authorized roots. This document is the owner's target contract; the current implementation is described in the [FileStorage plugin page](../../architecture/rust-agent/plugins/FileStorage.md). PluginFiles is a separate owner with its own contract and is not covered here; its calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

FileStorage owns:

- **Roots.** The process's authorized directories, their open handles and their order. A directory is addressable only because FileStorage opened it.
- **Generations.** One service instance admits calls, owns the descriptors it opened and releases them on close.
- **Exclusion.** Each file's lock key beside the file. A replacement, removal or update holds the key; another process holds the same key for the same file under every case and Unicode spelling the filesystem equates. FileStorage waits for the key until the caller cancels or the owner closes.
- **Atomic replacement.** A temporary file in the owner's namespace, flushed and renamed over the target, with the durability answer.
- **Pinned readers and document reads.** A whole document is read within the owner's limit in one operation; a pinned version of a larger file is addressed by a read id for bounded pages.
- **Path validation.** Absolute paths inside a root, one validated component per open, no link followed, the owner's own resource names excluded.

Callers keep:

- Document formats and versions: settings documents, `auth.json`, the plugin envelope, JSONL records.
- Which root and file a business uses, and the file names.
- What a business conflict means.
- Permission and approval: FileStorage never decides whether a call may run.
- Workspace, system and worktree facts: FileAccess owns them; FileStorage answers only whether an absolute path is inside a root.
- Files a plugin creates and manages itself under a root, such as SQLite's database, the model-log recorder's JSONL and the UI extension cache.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one document: settings, accounts, plugin records, a project's hooks file | No: composing it from the pin operations makes every caller hold a read id and release it on every error path | `read` |
| Read a large file in stable pages | No: a pin is the only way to keep one version across pages | `open_read`, `read_at`, `close_read` |
| Replace a document's bytes atomically | Yes: `write` whose source yields the whole content once | Business composes |
| Write content that arrives in pieces, including a stream an adapter produces | No: holding it whole is unbounded; the temporary file and the key must stay inside the owner | `write` with a source |
| Read, decide and write back while other processes may replace the file | No: between a read and a write another writer replaces the file, so only the owner can hold the file across the caller's decision | `update` with a change |
| Keep the replaced bytes before a destructive rewrite | Yes: read the current bytes and `write` a sibling copy; the caller owns the interleaving decision | Business composes; DimOAuth judges the window between the copy and the rewrite |
| Bring a host file the caller named into a root | No: the source is outside the roots and must be bounded and land atomically | `import` |
| Keep a document's file owner-only from creation (settings, accounts) | No: the mode is a file attribute the owner applies at creation and replacement | `permissions` on `write`, `import` and `update` |
| Write a Session file that `write` and `edit` may not modify | No: the read-only attribute is a file attribute the owner applies | `permissions` |
| Bound a host file import by the business limit (a Session attachment) | No: the caller's limit must stop the copy before it lands | `import` with `max_bytes` |
| Make a work directory tree and learn its absolute path | No: per-component link and alias validation is the owner's knowledge | `create_directory` |
| List one level and remove by a listed native name | No: the name's encoding and the single-component rule are the owner's knowledge | `list`, `remove` |
| Make a runtime-discovered directory addressable | No: the root set is the owner's state | `add_root` |
| Hand a root's directory to another module: a process working directory, a path quoted in history, a log directory | No: the root set is the owner's state | `root_path` |
| Learn whether an optional file exists (a project hooks file, a Settings source) | Yes: `read` answers none; `list` answers the parent's entries | Business composes |
| Decode or encode a document, choose its name and its root | Yes: formats and business paths stay with callers | Business |
| Append records to a log it owns (the model-log recorder) | Yes: the recorder owns the file; FileStorage supplies only the directory value | Business; the file is the recorder's, as SQLite's database is |
| Write a UI extension cache and then publish it | Yes: the cache belongs to UiExtensions; FileStorage reads it only on request | Business |
| Decide whether a read or write is allowed | Yes: Permission and Approval decide | Business |

An operation or parameter exists only where its row says No.

## Caller example

Reading one document: the owner opens, pages and releases the read id internally.

```rust
// None when the file is absent; too_large when it is over the document limit.
let Some(current) = storage.read(path, cancelled).await? else {
    return Ok(None);
};
```

Settings' compare-and-set: the change runs under the owner's file key, sees the current content and returns the next content or a business refusal.

```rust
storage.update(
    path,
    Arc::new(move |current: Option<Bytes>| {
        Box::pin(async move {
            let current = decode(current.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
            // The document's own revision is checked inside the helper; FileStorage's update
            // has no version or digest parameter, and a refusal here is a business error.
            let next = check_and_update(current, document)?; // business rules, including the document's own revision
            Ok(Bytes(encode(next)?))
        })
    }),
    Some(Permissions::Private),
    cancelled,
)
.await?;
```

SessionStore's write of bytes, and of a host path:

```rust
// `once` is a source that yields the bytes once and then none.
let size = storage.write(path, once(bytes), Some(Permissions::ReadOnly), cancelled.clone()).await?;
// A host file the caller named, without reading it first.
let size = storage.import(path, source, Some(MAX_FILE_BYTES), Some(Permissions::ReadOnly), cancelled).await?;
```

DimOAuth's reset: copy the current file, then rewrite it under the owner's key.

```rust
let current = storage.read(auth_path.clone(), cancelled.clone()).await?
    .ok_or(OAuthError::UnsupportedSchema)?;
// The backup is a business copy; the rewrite starts only after the copy is durable.
storage.write(backup_path, once(current), Some(Permissions::Preserve), cancelled.clone()).await?;
storage.update(
    auth_path,
    Arc::new(|current: Option<Bytes>| {
        Box::pin(async move {
            let mut document = parse(current.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
            document.remove(TOKENS);
            document.insert(STATE.into(), signed_out());
            Ok(Bytes(encode(document)?))
        })
    }),
    Some(Permissions::Private),
    cancelled,
)
.await?;
```

UiResources' publish, and a later page:

```rust
let Some(file) = storage.open_read(&self.ctx, request.path.clone(), cancelled.clone()).await
    .map_err(|_| ResourceError::service("resource_unavailable"))? else {
        return Err(ResourceError::service("resource_unavailable"));
    };
// The id belongs to this part's row; the publisher only decides which file.
let page = storage.read_at(&self.ctx, file.read_id.clone(), offset, cancelled).await?;
```

A module that only needs a directory value, such as the model-log recorder:

```rust
let home = storage.root_path(home_root)?;
let directory = format!("{home}/model-log"); // the recorder owns this file
```

## Own contract

### Dependencies

None. FileStorage injects no service and reads no other owner's state. The assembly provides the service and the initial roots; the trusted host adds a root at runtime. The provider is chosen by deployment ([resource faces §1](../resource-faces.md#1-scope)): whether a call reaches a local or a mounted instance is not a property of this name.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `add_root` | Make an existing absolute directory addressable | `{path}` | `{root}`; an existing root answers its id | Before the directory handle is opened | `invalid`, `closed`, `cancelled` |
| `root_path` | A root's absolute directory for another module | `{root}` | `{path}` | None; the owner completes the read | `invalid`, `closed` |
| `read` | Read a whole document within the owner's limit | `{path}` | `{bytes}` or none | While the file is opened | `invalid`, `too_large`, `closed`, `cancelled` |
| `open_read` | Pin a large file's current version for bounded random pages; the id is bound to the caller's row | `{path}` | `{read_id, size}` or none | While the file is opened | `invalid`, `closed`, `cancelled` |
| `read_at` | One bounded page of a pinned version | `{read_id, offset}` | `{bytes}`, empty at or past the end | The caller's token; nothing changes | `invalid`, `closed`, `cancelled` |
| `close_read` | Release a pin before the row unloads | `{read_id}` | `{}` | None; the owner completes the release | `invalid`, `closed` |
| `write` | Replace a file's bytes atomically from a source the owner pulls to its end | `{path, source, permissions?}` | `{size}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `import` | Copy a regular host file into a root atomically | `{path, source, max_bytes?, permissions?}` | `{size}` | Before the replacement | `invalid`, `too_large`, `missing_parent`, `unsynced`, `closed`, `cancelled` |
| `update` | Change a file's content while the owner holds the file across the caller's decision | `{path, change, permissions?}` | `{applied}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `too_large`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `create_directory` | Make the path an existing directory tree | `{path}` | `{path}` | Between levels; created ancestors stay, a retry is safe | `invalid`, `unsynced`, `closed`, `cancelled` |
| `list` | One level of a directory | `{path}` | `{entries}` or none when the directory is absent | Before the reply | `invalid`, `closed`, `cancelled` |
| `remove` | Make a file or a directory tree absent; an absent entry counts as removed | `{path}` or `{parent, name}` | `{}` | Before the entry is unlinked | `invalid`, `unsynced`, `closed`, `cancelled` |

`write`'s `source` is called repeatedly until it returns none: each call yields the next chunk, or a `Rejected` error that ends the write and leaves the target unchanged.

`update`'s `change` is called once while the file's key is held, with the current content or none when the file is absent, and returns the next content or a `Rejected` error. The owner writes the returned content atomically; `applied` is false when that content equals the current content, so a repeated write is idempotent. `change` must only compute: waiting on another owner, on network I/O or on FileStorage itself inside `change` holds the file for that whole wait. §7 allows a callback by reference, and the change is never called inside the Timeline's commit lock.

`permissions` is absent for "keep the existing mode; a new file gets the owner's default"; `private` is owner-only and `read_only` is the read-only attribute used for Session files. `max_bytes` is absent for the owner's limit. A listed name is passed back to `remove` unchanged. FileStorage waits for a file's key until the caller cancels or the owner closes; there is no fixed bound, so a caller that must not wait ends the call with its cancellation.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a relative path, a path outside every root, a reserved name, a name that is not one component, a directory where a file is required, a file where a directory is required, a link where a directory is required, an unknown or other-row id |
| `missing_parent` | Create the parent (`create_directory`) or fix the path |
| `too_large` | Use `open_read` when the caller needs the file in pages, or treat it as not a document |
| `rejected` | The change or the source refused or failed with its own `code` and `message`; act on that code, for example re-read and retry the business step on `conflict` |
| `unsynced` | The bytes are in place but durability is unconfirmed; a read sees them and a repeated call confirms the content; do not assume the earlier call failed |
| `cancelled` | Nothing changed; the call stopped waiting for the key or stopped before the replacement |
| `closed` | This generation is unloading; call the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `update` | Parent handle, file key, read the current content within the document limit, call `change`, write the temporary file, flush, rename, sync parent, release key | Before the rename: temporary file removed, target unchanged, key released, `cancelled`. After the rename: success or `unsynced`, the write is not undone | The change callback is revoked; the call ends `cancelled`; the same release applies | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Key and temporary file by FileStorage; the change's future by the calling row |
| `write` | Parent handle, file key, temporary file, pull the source to its end, flush, rename, sync parent, release key | The pull stops, the temporary file is removed, the target is unchanged, `cancelled` | The source callback is revoked; the pull fails; the temporary file is removed, `cancelled` | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Temporary file and key by FileStorage; the source's future by the calling row |
| `read` | Open the parent and the file without following links, read to the end within the document limit, release; no id crosses | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | Descriptor by FileStorage |
| `open_read`, `read_at`, `close_read` | Open the parent and the file without following links, pin the descriptor, register the id under the calling row; pages read at offsets from that descriptor | `open_read` before registration leaves nothing; a page ends `cancelled`; the pin stays until close or unload | The row's ids are released; a later page with the id is `invalid` | Every pin is released; later calls are `closed` | Descriptor by FileStorage; the id's lifetime by the calling row |
| `import` | Open the source without following links, require a regular file within the limit, copy into a temporary file, rename, sync parent | The temporary file is removed, the target is unchanged, the source closes | The call ends `cancelled` | Pending calls end `closed` | Source descriptor and temporary file by FileStorage |
| `remove` | Parent handle, the entry's key, remove depth first without following links, sync the parent | Before the unlink: nothing removed; a tree can be partly removed, a retry is safe | The call ends `cancelled` | Pending waits end `closed`; the generation releases keys | Entry removal by FileStorage |
| `create_directory` | One level at a time below the held parent, mode 0700, sync the parent | Between levels; created ancestors stay | The call ends `cancelled` | Pending calls end `closed` | Created directories by FileStorage |
| `list` | Open the directory, read one level, sort by name | Before the reply | The call ends `cancelled` | Pending calls end `closed` | Directory handle by FileStorage |
| `add_root` | Open the directory handle, append the root; an existing root answers its id | Before the handle is opened or appended | The call ends `cancelled` | New calls are refused; the append-only root table stays for the next generation, so a root id stays valid | Directory handle by FileStorage |

## Exceptions with reasons

- `import` reads a host path outside the roots: the caller names a file the user or model chose (an attachment); the owner checks that the source is a regular file, does not follow a link and enforces the byte limit, while the root constrains only the destination.
- The account marker `auth.json.lock` stays in its published TypeScript-compatible form: the account file is shared with the TypeScript subscriber, and both sides must lock the same file under the same marker protocol.
- Files another plugin owns under a root — SQLite's database, the model-log recorder's JSONL, the UI extension cache — are written by their owners, not through FileStorage; FileStorage answers `root_path` or reads such a file when asked. Ownership of a file follows the plugin that creates and manages it, not the root that contains it.
- `Rejected` carries only `code` and `message`: a callback error must cross languages, so it can come back only as data; the caller keeps its business error type inside its own callback, and FileStorage returns the code and message unchanged.
- Root ids are values that survive a reload: the root table is append-only, so a root id a caller kept stays valid across generations; the id is not a resource handle and carries no authority.

## Verification

Flow tests on the owner:

- `update` calls the change under the key; a change that returns `Rejected` leaves the file unchanged; an identical result reports `applied: false`; a cancel before the rename removes the temporary file; a failed parent sync reports `unsynced`.
- `write` pulls a multi-chunk source, commits atomically, removes the temporary file when the source fails or is revoked, and leaves the target when cancelled.
- `read` answers none for an absent file and `too_large` without partial bytes for a file over the document limit.
- A pin keeps its version while the path is replaced; another row's id is `invalid`; the row's unload releases its ids; the owner's close releases every pin.
- `import` over `max_bytes` copies nothing; `remove` of a tree with a file another writer holds waits, and cancelling ends the wait with `cancelled`; a retry after a partial removal finishes; a link is not followed.
- A call in flight when the generation closes ends `closed`; a root id stays valid across a reload.

Material uncertainty: a change that waits on another owner or on I/O holds the file's key for that wait. The contract forbids it; review checks each caller's change.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | FileStorage is one process part. It writes no Timeline event and folds no state, so it is not a Capability. It injects no service; the process assembly provides it. |
| §2 scopes | Process. Callers are process or Session parts and use the service as a process dependency. No branch or Session state is held. |
| §3 orthogonality | No other owner's state is read or changed. A caller that needs a new file operation gets a general operation here or composes one. |
| §4 rule 1 | No command event: FileStorage writes no Timeline fact. |
| §4 rule 2 | No handoff: the bytes and the key stay with FileStorage. |
| §4 rule 3 | No before callback: the `update` change runs under the file's own key, never inside the Timeline's commit lock. |
| §4 rule 4 | Core and assembly carry no business meaning; no document format, permission or approval appears in this contract. |
| §4 rule 5 | No batches. |
| §4 rule 6 | File content reaches the model only through tool results; the tools own that path, and FileStorage only returns bytes to the owner that asked. |
| §4 rule 7 | Callers use these operations; no caller reads a descriptor, a key or a generation. |
| §4 rule 8 | No infrastructure change. The one unmet runtime requirement is [#861](https://github.com/arcships/dim/issues/861), resolving a Session's process-scope service by its resource node; it stays with the runtime and is stated in [resource faces §8](../resource-faces.md#8-requirements-raised-to-mechanism-owners). |
| §4 rule 9 | No Timeline fact. |
| §7 callbacks and ids | The write source and the update change cross by reference as §7 allows; the read id crosses and is bound to the calling row. No object or handle crosses. |
| §7 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Process scope: roots, descriptors, keys and generations outlive Sessions and are shared across them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; no adapter or facade exists beside it. |
| ADR 002 D3 | Dependencies point from extensions to the core; FileStorage depends on nothing and is a process service they may depend on. |
| ADR 002 D4 | Roots are structural: the assembly provides the initial ones and the trusted host adds one at runtime; no business setting is involved. |
| ADR 002 D5 | The contract is language-neutral: operations, data, callbacks and ids; the Rust binding is the appendix below. |


## Requirements raised to mechanism owners

None new. [#861](https://github.com/arcships/dim/issues/861) remains the runtime's requirement for reaching a Session's process-scope instance; this contract assumes the host resolves the instance and keeps the name's shape unchanged.

## Binding appendix

The service is marked on the owner type; the wire name stays `fsHost`. The `FsHost` wrapper type no longer exists. `caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, it is not a request field, and no caller sends or names a row. `CancelledFn` is the binding's cancellation argument, also not a request field.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use rutis::Ctx;
use std::sync::Arc;

pub type RootId = String;
pub type ReadId = String;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Bytes(#[serde(with = "base64_bytes")] pub Vec<u8>); // a small local serde module; crosses as base64 text

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permissions {
    #[default]
    Preserve,
    Private,
    ReadOnly,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadHandle {
    pub read_id: ReadId,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReply {
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: Name,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Name {
    Text(String),
    Unix { unix: Vec<u8> },
    Windows { windows: Vec<u16> },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RemoveTarget {
    Path { path: String },
    Entry { parent: String, name: Name },
}

/// The caller's own refusal or failure from a source or a change, carried unchanged.
#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Rejected {
    pub code: String,
    pub message: String,
}

impl Rejected {
    pub fn new(code: &str, message: &str) -> Self {
        Self { code: code.to_owned(), message: message.to_owned() }
    }
}

/// A failed callback arrives as the wire's error; the caller reads its message.
impl From<rutis::host::CallError> for Rejected {
    fn from(error: rutis::host::CallError) -> Self {
        Self::new("rejected", error.message())
    }
}

/// The next chunk of a write, or none at the end.
pub type SourceFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Option<Bytes>, Rejected>> + Send + Sync>;

/// The current content (none when absent); the next content or a refusal.
pub type ChangeFn =
    Arc<dyn Fn(Option<Bytes>) -> BoxFuture<'static, Result<Bytes, Rejected>> + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{0}")]
    Invalid(String),
    #[error("parent directory is missing: {0}")]
    MissingParent(String),
    #[error("{path} is {size} bytes, over the document limit")]
    TooLarge { path: String, size: u64 },
    #[error("{code}: {message}")]
    Rejected { code: String, message: String },
    #[error("{0} is in place but its durability is unconfirmed")]
    Unsynced(String),
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the file service is unloading")]
    Closed,
}

impl PublicService for FileStorage {}

#[rutis::service(name = "fsHost")]
impl FileStorage {
    pub async fn add_root(&self, path: String, cancelled: CancelledFn) -> Result<RootId, FileError> {
        // ...
    }

    pub fn root_path(&self, root: RootId) -> Result<String, FileError> {
        // ...
    }

    pub async fn read(
        &self,
        path: String,
        cancelled: CancelledFn,
    ) -> Result<Option<Bytes>, FileError> {
        // ...
    }

    pub async fn open_read(
        &self,
        caller: &Ctx,
        path: String,
        cancelled: CancelledFn,
    ) -> Result<Option<ReadHandle>, FileError> {
        // ...
    }

    pub async fn read_at(
        &self,
        caller: &Ctx,
        read_id: ReadId,
        offset: u64,
        cancelled: CancelledFn,
    ) -> Result<Bytes, FileError> {
        // ...
    }

    pub async fn close_read(&self, caller: &Ctx, read_id: ReadId) -> Result<(), FileError> {
        // ...
    }

    pub async fn write(
        &self,
        path: String,
        source: SourceFn,
        permissions: Option<Permissions>,
        cancelled: CancelledFn,
    ) -> Result<u64, FileError> {
        // ...
    }

    pub async fn import(
        &self,
        path: String,
        source: String,
        max_bytes: Option<u64>,
        permissions: Option<Permissions>,
        cancelled: CancelledFn,
    ) -> Result<u64, FileError> {
        // ...
    }

    pub async fn update(
        &self,
        path: String,
        change: ChangeFn,
        permissions: Option<Permissions>,
        cancelled: CancelledFn,
    ) -> Result<UpdateReply, FileError> {
        // ...
    }

    pub async fn create_directory(
        &self,
        path: String,
        cancelled: CancelledFn,
    ) -> Result<String, FileError> {
        // ...
    }

    pub async fn list(
        &self,
        path: String,
        cancelled: CancelledFn,
    ) -> Result<Option<Vec<Entry>>, FileError> {
        // ...
    }

    pub async fn remove(&self, target: RemoveTarget, cancelled: CancelledFn) -> Result<(), FileError> {
        // ...
    }
}
```

`CancelledFn` is the crate's existing cancellation argument ([`host::CancelledFn`](../../../crates/dim-agent-capabilities/src/host.rs)). `Bytes` crosses as base64 text; `Name` as a string or a native-bytes object; `RootId`, `ReadId` and sizes as strings and numbers. Requests carry callbacks by reference only where the operation needs the caller's own step — the write source and the update change; every other argument and reply is data.
