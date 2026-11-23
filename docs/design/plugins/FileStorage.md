# FileStorage

Scope: process. Issue: #2016.

FileStorage owns byte storage under explicitly authorized roots. This document is the owner's target contract; the current implementation is described in the [FileStorage plugin page](../../architecture/rust-agent/plugins/FileStorage.md). PluginFiles is a separate owner with its own contract and is not covered here; its calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

FileStorage owns:

- **Roots.** The process's authorized directories, their open handles and their order. A directory is addressable only because FileStorage opened it.
- **Generations.** One service instance admits calls, owns the descriptors it opened and releases them on close.
- **Exclusion.** Each file's lock key beside the file. A replacement or removal holds the key; another process holds the same key for the same file under every case and Unicode spelling the filesystem equates.
- **Atomic replacement.** A temporary file in the owner's namespace, flushed and renamed over the target, with the durability answer.
- **Pinned readers.** A file version pinned for bounded random reads and addressed by a read id.
- **Path validation.** Absolute paths inside a root, one validated component per open, no link followed, the owner's own resource names excluded.

Callers keep:

- Document formats and versions: settings documents, `auth.json`, the plugin envelope, JSONL records.
- Which root and file a business uses, and the file names.
- What a `busy` answer means for a flow, and what a business conflict means.
- Permission and approval: FileStorage never decides whether a call may run.
- Files a plugin creates and manages itself under a root, such as SQLite's database, the model-log recorder's JSONL and the UI extension cache.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one document: settings, accounts, plugin records, a project's hooks file | Yes: `open_read`, `read_at` to the end, `close_read`; the pin keeps one version | Business composes; each owner keeps one small read helper |
| Read a large file in stable pages | No: a pin is the only way to keep one version across pages | `open_read`, `read_at`, `close_read` |
| Replace a document's bytes atomically | Yes: `write_stream` whose source yields the whole content once | Business composes |
| Write content an adapter produces incrementally | No: holding it whole is unbounded and cannot be atomic | `write_stream` |
| Read, decide and write back while other processes may write | No: between a read and a write another writer replaces the file | `update` |
| Keep the replaced bytes before a destructive rewrite | No: the copy must be the version replaced under the same exclusion | `update` with `keep_replaced_as` |
| Bring a host file the caller named into a root | No: the source is outside the roots and must be bounded and land atomically | `import` |
| Keep a document's file owner-only from creation (settings, accounts) | No: the mode is a file attribute the owner applies at creation and replacement | `permissions` on `write_stream`, `import` and `update` |
| Write a Session file that `write` and `edit` may not modify | No: the read-only attribute is a file attribute the owner applies | `permissions` |
| Bound a host file import by the business limit (a Session attachment) | No: the caller's limit must stop the copy before it lands | `import` with `max_bytes` |
| Make a work directory tree and learn its absolute path | No: per-component link and alias validation is the owner's knowledge | `create_directory` |
| List one level and remove by a listed native name | No: the name's encoding and the single-component rule are the owner's knowledge | `list`, `remove` |
| Make a runtime-discovered directory addressable | No: the root set is the owner's state | `add_root` |
| Hand a root's directory to another module: a process working directory, a path quoted in history, a log directory | No: the root set is the owner's state | `root_path` |
| Learn whether an optional file exists (a project hooks file, a Settings source) | Yes: `open_read` answers none; `list` answers the parent's entries | Business composes |
| Decode or encode a document, choose its name and its root | Yes: formats and business paths stay with callers | Business |
| Decide when to try again after `busy` | Yes: `busy` is a business answer | Business |
| Append records to a log it owns (the model-log recorder) | Yes: the recorder owns the file; FileStorage supplies only the directory value | Business; the file is the recorder's, as SQLite's database is |
| Write a UI extension cache and then publish it | Yes: the cache belongs to UiExtensions; FileStorage reads it only on request | Business |
| Decide whether a read or write is allowed | Yes: Permission and Approval decide | Business |

An operation or parameter exists only where its row says No.

## Caller example

Reading one document, the composition every document owner uses:

```rust
async fn read_document(storage: &FileStorage, ctx: &Ctx, path: &str, cancelled: CancelledFn)
    -> Result<Option<Bytes>, FileError>
{
    let Some(file) = storage.open_read(ctx, path.to_owned(), cancelled.clone()).await? else {
        return Ok(None);
    };
    let mut content = Vec::with_capacity(file.size as usize);
    while (content.len() as u64) < file.size {
        let page = storage.read_at(ctx, file.read_id.clone(), content.len() as u64, cancelled.clone()).await?;
        if page.is_empty() {
            break;
        }
        content.extend_from_slice(&page);
    }
    storage.close_read(ctx, file.read_id).await?;
    Ok(Some(Bytes(content)))
}
```

Settings' compare-and-set, with the revision check inside the change:

```rust
let root = storage.add_root(cwd.clone(), cancelled.clone()).await?;
let path = format!("{}/settings.json", storage.root_path(root)?);
let reply = storage.update(
    path,
    Some(Permissions::Private),
    None,
    Arc::new(move |current: Option<Bytes>| {
        Box::pin(async move {
            let current = decode(current.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
            if current.revision != expected {
                return Err(Rejected::new("conflict", "the document changed since it was read"));
            }
            let next = check_and_update(current, document)?; // business rules
            Ok(Some(Bytes(encode(next)?)))
        })
    }),
    cancelled,
).await?;
let snapshot = decode(reply.content.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
```

SessionStore's put, for bytes and for a host path:

```rust
let directory = storage.create_directory(format!("{root}/.session-files/{id}"), cancelled.clone()).await?;
let path = format!("{directory}/{name}");
let size = match content {
    // `once` is a source that yields these bytes once and then none.
    Content::Bytes(bytes) => {
        storage.write_stream(path, Some(Permissions::ReadOnly), once(bytes), cancelled).await?
    }
    Content::Path(source) => {
        storage.import(path, source, Some(MAX_FILE_BYTES), Some(Permissions::ReadOnly), cancelled).await?
    }
};
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
| `add_root` | Make an existing absolute directory addressable | `{path}` | `{root}`; an existing root answers its id | Before the directory handle is opened | `invalid`, `closed` |
| `root_path` | A root's absolute directory for another module | `{root}` | `{path}` | None; the owner completes the read | `invalid`, `closed` |
| `open_read` | Pin the file's current version for bounded random reads; the id is bound to the caller's row | `{path}` | `{read_id, size}` or none | While the file is opened | `invalid`, `closed`, `cancelled` |
| `read_at` | One bounded page of a pinned version | `{read_id, offset}` | `{bytes}`, empty at or past the end | The caller's token; nothing changes | `invalid`, `closed`, `cancelled` |
| `close_read` | Release a pin before the row unloads | `{read_id}` | `{}` | None; the owner completes the release | `invalid`, `closed` |
| `write_stream` | Replace a file's bytes atomically from a source the owner pulls to its end | `{path, permissions?, source}` | `{size}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `busy`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `import` | Copy a regular host file into a root atomically | `{path, source, max_bytes?, permissions?}` | `{size}` | Before the replacement | `invalid`, `too_large`, `missing_parent`, `busy`, `unsynced`, `closed`, `cancelled` |
| `update` | Change a file's content while no other writer replaces it | `{path, permissions?, keep_replaced_as?, change}` | `{applied, content}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `too_large`, `busy`, `backup_exists`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `create_directory` | Make the path an existing directory tree | `{path}` | `{path}` | Between levels; created ancestors stay, a retry is safe | `invalid`, `unsynced`, `closed`, `cancelled` |
| `list` | One level of a directory | `{path}` | `{entries}` or none when the directory is absent | Before the reply | `invalid`, `closed`, `cancelled` |
| `remove` | Make a file or a directory tree absent; an absent entry counts as removed | `{path}` or `{parent, name}` | `{}` | Before the entry is unlinked | `invalid`, `busy`, `unsynced`, `closed`, `cancelled` |

`change` receives the current content, or none when the file is absent, and returns `Some(content)` to replace the file or `None` to keep it. A change or a write source that refuses or fails returns `Rejected` with its own code and message; the owner forwards it as `rejected`. `change` runs while the file's key is held and must only compute: waiting on another owner or on network I/O inside `change` holds the file for that whole wait, and calling FileStorage again inside `change` can wait on the file being held.

`permissions` is absent for "keep the existing mode; a new file gets the owner's default"; `private` is owner-only and `read_only` is the read-only attribute used for Session files. `max_bytes` is absent for the owner's limit. A listed name is passed back to `remove` unchanged.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a relative path, a path outside every root, a reserved name, a name that is not one component, a directory where a file is required, a file where a directory is required, a link where a directory is required, an unknown or other-row id |
| `missing_parent` | Create the parent (`create_directory`) or fix the path |
| `too_large` | Read it in pages (`open_read`) or treat it as not a document |
| `busy` | Try the same call again later; the input stays valid |
| `backup_exists` | The rewrite is refused; the existing copy needs the owner's decision |
| `rejected` | The change or the write source refused or failed with its own `code` and `message`; act on that code, for example re-read and retry the business step on `conflict` |
| `unsynced` | The bytes are in place but durability is unconfirmed; a read sees them and a repeated call confirms the content; do not assume the earlier call failed |
| `cancelled` | Nothing changed unless the reply says otherwise |
| `closed` | This generation is unloading; call the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `update` | Parent handle, file key, read the current content within the document limit, run `change`, write temporary file, flush, rename, sync parent, release key | Before the rename: temporary file removed, target unchanged, key released, `cancelled`. After the rename: success or `unsynced`, the change is not undone | The change callback is revoked; the call ends `cancelled`; the same release applies | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Key and temporary file by FileStorage; the change's future by the calling row |
| `open_read`, `read_at`, `close_read` | Open the parent and the file without following links, pin the descriptor, register the id under the calling row; pages read at offsets from that descriptor | `open_read` before registration leaves nothing; a page ends `cancelled`; the pin stays until close or unload | The row's ids are released; a later page with the id is `invalid` | Every pin is released; later calls are `closed` | Descriptor by FileStorage; the id's lifetime by the calling row |
| `write_stream` | Parent handle, key, temporary file, pull the source to its end, flush, rename, sync parent, release key | The pull stops, the temporary file is removed, the target is unchanged, `cancelled` | The source callback is revoked; the pull fails; the temporary file is removed, `cancelled` | As `update` | Temporary file and key by FileStorage; the source future by the calling row |
| `import` | Open the source without following links, require a regular file within the limit, copy into a temporary file, rename, sync parent | The temporary file is removed, the target is unchanged, the source closes | As `write_stream` | As `update` | Source descriptor and temporary file by FileStorage |
| `remove` | Parent handle, the entry's key, remove depth first without following links, sync the parent | Before the unlink: nothing removed; a tree can be partly removed, a retry is safe | The call ends `cancelled` | Pending waits end `closed`; the generation releases keys | Entry removal by FileStorage |
| `create_directory` | One level at a time below the held parent, mode 0700, sync the parent | Between levels; created ancestors stay | The call ends `cancelled` | Pending calls end `closed` | Created directories by FileStorage |
| `list` | Open the directory, read one level, sort by name | Before the reply | The call ends `cancelled` | Pending calls end `closed` | Directory handle by FileStorage |
| `add_root` | Open the directory handle, append the root; an existing root answers its id | Before the handle is opened or appended | The call ends `cancelled` | New calls are refused; the append-only root table stays for the next generation, so a root id stays valid | Directory handle by FileStorage |

## Exceptions with reasons

- `import` reads a host path outside the roots: the caller names a file the user or model chose (an attachment); the owner checks that the source is a regular file, does not follow a link and enforces the byte limit, while the root constrains only the destination.
- The account marker `auth.json.lock` stays in its published TypeScript-compatible form: the account file is shared with the TypeScript subscriber, and both sides must lock the same file under the same marker protocol.
- Files another plugin owns under a root — SQLite's database, the model-log recorder's JSONL, the UI extension cache — are written by their owners, not through FileStorage; FileStorage answers `root_path` or reads such a file when asked. Ownership of a file follows the plugin that creates and manages it, not the root that contains it.
- Root ids are values that survive a reload: the root table is append-only, so a root id a caller kept stays valid across generations; the id is not a resource handle and carries no authority.

## Verification

Flow tests on the owner:

- `update` reports `rejected` with the change's code, `backup_exists` when the replaced copy exists, `unsynced` after a failed parent sync, and leaves the target unchanged when the change or a cancel stops before the rename.
- A pin keeps its version while the path is replaced; another row's id is `invalid`; the row's unload releases its ids; the owner's close releases every pin.
- `write_stream` removes its temporary file and leaves the target when the source fails or is revoked; `import` over `max_bytes` copies nothing.
- `remove` of a tree with a file another writer holds answers `busy`; a retry after a partial removal finishes; a link is not followed.
- A call in flight when the generation closes ends `closed`; a root id stays valid across a reload.

Material uncertainty: a change that waits on another owner or on I/O holds the file's key for that wait. The contract forbids it; review checks each caller's change, and a flow test covers the pattern the callers use.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | FileStorage is one process part. It writes no Timeline event and folds no state, so it is not a Capability. It injects no service; the process assembly provides it. |
| §2 scopes | Process. Callers are process or Session parts and use the service as a process dependency. No branch or Session state is held. |
| §3 orthogonality | No other owner's state is read or changed. A caller that needs a new file operation gets a general operation here or composes one. |
| §4 rule 1 | No command event: FileStorage writes no Timeline fact. |
| §4 rule 2 | No handoff: the bytes and the lock stay with FileStorage. |
| §4 rule 3 | No before callback and no commit-lock work; `update`'s change runs under the file's own key, not the Timeline's commit lock. |
| §4 rule 4 | Core and assembly carry no business meaning; no document format, permission or approval appears in this contract. |
| §4 rule 5 | No batches. |
| §4 rule 6 | File content reaches the model only through tool results; the tools own that path, and FileStorage only returns bytes to the owner that asked. |
| §4 rule 7 | Callers use these operations; no caller reads a descriptor, a key or a generation. |
| §4 rule 8 | No infrastructure change. The one unmet runtime requirement is [#861](https://github.com/arcships/dim/issues/861), resolving a Session's process-scope service by its resource node; it stays with the runtime and is stated in [resource faces §8](../resource-faces.md#8-requirements-raised-to-mechanism-owners). |
| §4 rule 9 | No Timeline fact. |
| ADR 002 D1 | Process scope: roots, descriptors, keys and generations outlive Sessions and are shared across them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; no adapter or facade exists beside it. |
| ADR 002 D3 | Dependencies point from extensions to the core; FileStorage depends on nothing and is a process service they may depend on. |
| ADR 002 D4 | Roots are structural: the assembly provides the initial ones and the trusted host adds one at runtime; no business setting is involved. |
| ADR 002 D5 | The contract is language-neutral: operations, data and ids; the Rust binding is the appendix below. |

Pending (see #2015 待决): whether the file tools' per-turn use of this service is a hot-path exception. The operation shapes here do not depend on that decision.

## Requirements raised to mechanism owners

None new. [#861](https://github.com/arcships/dim/issues/861) remains the runtime's requirement for reaching a Session's process-scope instance; this contract assumes the host resolves the instance and keeps the name's shape unchanged.

## Binding appendix

The service is marked on the owner type; the wire name stays `fsHost`. The `FsHost` wrapper type no longer exists.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::host::{BoxFuture, CallError};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReply {
    pub applied: bool,
    pub content: Option<Bytes>,
}

/// The change's own refusal, carried to the caller unchanged.
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

/// A failed change arrives as the wire's error; the caller reads its message.
impl From<CallError> for Rejected {
    fn from(error: CallError) -> Self {
        Self::new("rejected", error.message())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{0}")]
    Invalid(String),
    #[error("parent directory is missing: {0}")]
    MissingParent(String),
    #[error("{path} is {size} bytes, over the document limit")]
    TooLarge { path: String, size: u64 },
    #[error("{0} is being written by another writer")]
    Busy(String),
    #[error("the replaced copy already exists: {0}")]
    BackupExists(String),
    #[error("{code}: {message}")]
    Rejected { code: String, message: String },
    #[error("{0} is in place but its durability is unconfirmed")]
    Unsynced(String),
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the file service is unloading")]
    Closed,
}

/// The next chunk of a write, or none at the end.
pub type SourceFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Option<Bytes>, Rejected>> + Send + Sync>;

/// The current content (none when absent); `Some` replaces, `None` keeps.
pub type ChangeFn =
    Arc<dyn Fn(Option<Bytes>) -> BoxFuture<'static, Result<Option<Bytes>, Rejected>> + Send + Sync>;

impl PublicService for FileStorage {}

#[rutis::service(name = "fsHost")]
impl FileStorage {
    pub async fn add_root(&self, path: String, cancelled: CancelledFn) -> Result<RootId, FileError> {
        // ...
    }

    pub fn root_path(&self, root: RootId) -> Result<String, FileError> {
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

    pub async fn write_stream(
        &self,
        path: String,
        permissions: Option<Permissions>,
        source: SourceFn,
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
        permissions: Option<Permissions>,
        keep_replaced_as: Option<String>,
        change: ChangeFn,
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

`CancelledFn` is the crate's existing cancellation argument ([`host::CancelledFn`](../../../crates/dim-agent-capabilities/src/host.rs)). `Bytes` crosses as base64 text; `Name` as a string or a native-bytes object; `RootId`, `ReadId` and sizes as strings and numbers.
