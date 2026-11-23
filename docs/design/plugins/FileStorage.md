# FileStorage

Scope: process. Issue: #2016.

FileStorage owns byte storage under explicitly authorized roots. This document is the owner's target contract; the current implementation is described in the [FileStorage plugin page](../../architecture/rust-agent/plugins/FileStorage.md). PluginFiles is a separate owner with its own contract and is not covered here; its calls appear in the delivery tables as caller rows.

## Use and scope: responsibility split

FileStorage owns:

- **Roots.** The process's authorized directories, their open handles and their order. A directory is addressable only because FileStorage opened it.
- **Generations.** One service instance admits calls, owns the descriptors it opened and releases them on close.
- **Exclusion.** Each file's lock key beside the file. A replacement, removal or write session holds the key; another process holds the same key for the same file under every case and Unicode spelling the filesystem equates.
- **Atomic replacement.** A temporary file in the owner's namespace, flushed and renamed over the target, with the durability answer.
- **Pinned readers and write sessions.** A pinned file version for bounded random reads, and a write session under a held key, each addressed by an id.
- **Path validation.** Absolute paths inside a root, one validated component per open, no link followed, the owner's own resource names excluded.

Callers keep:

- Document formats and versions: settings documents, `auth.json`, the plugin envelope, JSONL records.
- Which root and file a business uses, and the file names.
- What a `busy` or a `conflict` answer means for a flow.
- Permission and approval: FileStorage never decides whether a call may run.
- Workspace, system and worktree facts: FileAccess owns them; FileStorage answers only whether an absolute path is inside a root.
- Files a plugin creates and manages itself under a root, such as SQLite's database, the model-log recorder's JSONL and the UI extension cache.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one document: settings, accounts, plugin records, a project's hooks file | Yes: `open_read`, `read_at` to the end, `close_read`; the pin keeps one version | Business composes; each owner keeps one small read helper |
| Read a large file in stable pages | No: a pin is the only way to keep one version across pages | `open_read`, `read_at`, `close_read` |
| Replace a document's bytes atomically | Yes: a write session | Business composes |
| Write content an adapter produces incrementally | No: holding it whole is unbounded and cannot be atomic | `open_write`, `write_chunk`, `commit_write`, `abort_write` |
| Write only if the file is still the version that was read | No: the owner is the only one that can compare and replace without another writer slipping in | `update` with `expected` |
| Keep the replaced bytes before a destructive rewrite | No: the copy must be the version replaced under the same exclusion | `update` with `keep_replaced_as` |
| Bring a host file the caller named into a root | No: the source is outside the roots and must be bounded and land atomically | `import` |
| Keep a document's file owner-only from creation (settings, accounts) | No: the mode is a file attribute the owner applies at creation and replacement | `permissions` on `open_write`, `import` and `update` |
| Write a Session file that `write` and `edit` may not modify | No: the read-only attribute is a file attribute the owner applies | `permissions` |
| Bound a host file import by the business limit (a Session attachment) | No: the caller's limit must stop the copy before it lands | `import` with `max_bytes` |
| Make a work directory tree and learn its absolute path | No: per-component link and alias validation is the owner's knowledge | `create_directory` |
| List one level and remove by a listed native name | No: the name's encoding and the single-component rule are the owner's knowledge | `list`, `remove` |
| Make a runtime-discovered directory addressable | No: the root set is the owner's state | `add_root` |
| Hand a root's directory to another module: a process working directory, a path quoted in history, a log directory | No: the root set is the owner's state | `root_path` |
| Learn whether an optional file exists (a project hooks file, a Settings source) | Yes: `open_read` answers none; `list` answers the parent's entries | Business composes |
| Decode or encode a document, choose its name and its root | Yes: formats and business paths stay with callers | Business |
| Decide when to try again after `busy` or `conflict` | Yes: both are business answers | Business |
| Append records to a log it owns (the model-log recorder) | Yes: the recorder owns the file; FileStorage supplies only the directory value | Business; the file is the recorder's, as SQLite's database is |
| Write a UI extension cache and then publish it | Yes: the cache belongs to UiExtensions; FileStorage reads it only on request | Business |
| Decide whether a read or write is allowed | Yes: Permission and Approval decide | Business |

An operation or parameter exists only where its row says No. No caller code runs while FileStorage holds a file key: every operation takes data and returns data.

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

Settings' compare-and-set: read, compute the digest of what was read, and write only for that version.

```rust
let current = read_document(&storage, &ctx, &path, cancelled.clone()).await?;
let expected = match &current {
    Some(bytes) => Expected::Digest { digest: sha256_hex(&bytes.0) }, // the version that was read
    None => Expected::Absent,                                          // create only if still absent
};
match storage
    .update(path, expected, Bytes(next), Some(Permissions::Private), None, cancelled)
    .await
{
    Ok(UpdateReply { applied: _ }) => Ok(SettingsSnapshot { revision, document }),
    Err(FileError::Conflict(_)) => Err(SettingsError::Conflict), // re-read and retry the business step
    Err(error) => Err(error.into()),
}
```

SessionStore's write of bytes, and of a host path:

```rust
// `write_bytes` opens a session, writes the chunks, commits, and aborts on failure.
let size = write_bytes(&storage, &ctx, path, bytes, Some(Permissions::ReadOnly), cancelled.clone()).await?;
// A host file the caller named, without reading it first.
let size = storage.import(path, source, Some(MAX_FILE_BYTES), Some(Permissions::ReadOnly), cancelled).await?;
```

`write_bytes` is the caller-side composition of the write session:

```rust
async fn write_bytes(storage: &FileStorage, ctx: &Ctx, path: String, bytes: Vec<u8>,
                     permissions: Option<Permissions>, cancelled: CancelledFn) -> Result<u64, FileError> {
    let write = storage.open_write(ctx, path, permissions, cancelled.clone()).await?;
    let result = async {
        for chunk in bytes.chunks(MAX_CHUNK) {
            storage.write_chunk(ctx, write.write_id.clone(), Bytes(chunk.to_vec()), cancelled.clone()).await?;
        }
        storage.commit_write(ctx, write.write_id.clone()).await
    }
    .await;
    if result.is_err() {
        let _ = storage.abort_write(ctx, write.write_id).await;
    }
    result
}
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
| `open_read` | Pin the file's current version for bounded random reads; the id is bound to the caller's row | `{path}` | `{read_id, size}` or none | While the file is opened | `invalid`, `closed`, `cancelled` |
| `read_at` | One bounded page of a pinned version | `{read_id, offset}` | `{bytes}`, empty at or past the end | The caller's token; nothing changes | `invalid`, `closed`, `cancelled` |
| `close_read` | Release a pin before the row unloads | `{read_id}` | `{}` | None; the owner completes the release | `invalid`, `closed` |
| `open_write` | Begin replacing a file's bytes; the owner holds the target's key and a temporary file for the session | `{path, permissions?}` | `{write_id}` | While the session opens | `invalid`, `missing_parent`, `busy`, `closed`, `cancelled` |
| `write_chunk` | Append one chunk to the session's temporary file | `{write_id, bytes}` | `{}` | Before the chunk is written | `invalid`, `closed`, `cancelled` |
| `commit_write` | Publish the session's bytes atomically and release the session | `{write_id}` | `{size}` | Before the rename; the temporary file is removed and the session ends | `invalid`, `unsynced`, `closed`, `cancelled` |
| `abort_write` | Discard the session's bytes and release it | `{write_id}` | `{}` | None; the owner completes the discard | `invalid`, `closed` |
| `import` | Copy a regular host file into a root atomically | `{path, source, max_bytes?, permissions?}` | `{size}` | Before the replacement | `invalid`, `too_large`, `missing_parent`, `busy`, `unsynced`, `closed`, `cancelled` |
| `update` | Replace a file's content only if it is still the expected version | `{path, expected, content, permissions?, keep_replaced_as?}` | `{applied}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `too_large`, `busy`, `conflict`, `backup_exists`, `unsynced`, `closed`, `cancelled` |
| `create_directory` | Make the path an existing directory tree | `{path}` | `{path}` | Between levels; created ancestors stay, a retry is safe | `invalid`, `unsynced`, `closed`, `cancelled` |
| `list` | One level of a directory | `{path}` | `{entries}` or none when the directory is absent | Before the reply | `invalid`, `closed`, `cancelled` |
| `remove` | Make a file or a directory tree absent; an absent entry counts as removed | `{path}` or `{parent, name}` | `{}` | Before the entry is unlinked | `invalid`, `busy`, `unsynced`, `closed`, `cancelled` |

`expected` is `{kind: "absent"}` when the file must not exist, or `{kind: "digest", digest}` where `digest` is the lowercase hex SHA-256 of the file bytes the caller read. `update` hashes the current content and replaces it only when the digest matches; otherwise it answers `conflict` and changes nothing. `applied` is false when the new content equals the current content, so a repeated write is idempotent.

`permissions` is absent for "keep the existing mode; a new file gets the owner's default"; `private` is owner-only and `read_only` is the read-only attribute used for Session files. `max_bytes` is absent for the owner's limit. A listed name is passed back to `remove` unchanged. A write session that is neither committed nor aborted is settled when the calling row unloads or the owner closes.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a relative path, a path outside every root, a reserved name, a name that is not one component, a directory where a file is required, a file where a directory is required, a link where a directory is required, an unknown, other-row or ended id |
| `missing_parent` | Create the parent (`create_directory`) or fix the path |
| `too_large` | Read it in pages (`open_read`) or write it through a session |
| `busy` | Try the same call again later; the input stays valid |
| `conflict` | The file is not the expected version; re-read it and retry the business step |
| `backup_exists` | The rewrite is refused; the existing copy needs the owner's decision |
| `unsynced` | The bytes are in place but durability is unconfirmed; a read sees them and a repeated call confirms the content; do not assume the earlier call failed |
| `cancelled` | Nothing changed unless the reply says otherwise |
| `closed` | This generation is unloading; call the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `open_write`, `write_chunk` | Open the parent, hold the file key, create the temporary file; each chunk is appended to it | A cancelled chunk writes nothing; the session stays usable | The row's sessions are aborted: temporary file removed, key released | New calls are refused; every session is aborted; in-flight chunks end `closed` or `cancelled` | Key and temporary file by FileStorage; the id's lifetime by the calling row |
| `commit_write`, `abort_write` | Commit flushes and renames the temporary file, syncs the parent and releases the key; abort removes the temporary file and releases the key | Commit before the rename discards the session; abort completes | The id is gone; a later call is `invalid` | The session ends as aborted; a commit already past the rename reports success or `unsynced` | Temporary file and key by FileStorage |
| `update` | Parent handle, file key, hash the current content and compare with `expected`; a mismatch answers `conflict` and changes nothing; a match writes the replaced copy when asked, then the temporary file, flush, rename, sync parent, release key | Before the rename: temporary file removed, target unchanged, key released, `cancelled`. After the rename: success or `unsynced`, the write is not undone | The call ends `cancelled`; the same release applies | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Key and temporary file by FileStorage |
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
- Root ids are values that survive a reload: the root table is append-only, so a root id a caller kept stays valid across generations; the id is not a resource handle and carries no authority.

## Verification

Flow tests on the owner:

- `update` answers `conflict` without changing the file when the digest does not match, reports `backup_exists` when the replaced copy exists, reports `unsynced` after a failed parent sync, and reports `applied: false` for an identical write.
- A write session appends chunks, commits atomically, and aborts on request; a failed chunk or a row unload leaves the target unchanged and no temporary file.
- A pin keeps its version while the path is replaced; another row's id is `invalid`; the row's unload releases its ids; the owner's close releases every pin and aborts every session.
- `import` over `max_bytes` copies nothing; `remove` of a tree with a file another writer holds answers `busy`; a retry after a partial removal finishes; a link is not followed.
- A call in flight when the generation closes ends `closed`; a root id stays valid across a reload.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | FileStorage is one process part. It writes no Timeline event and folds no state, so it is not a Capability. It injects no service; the process assembly provides it. |
| §2 scopes | Process. Callers are process or Session parts and use the service as a process dependency. No branch or Session state is held. |
| §3 orthogonality | No other owner's state is read or changed. A caller that needs a new file operation gets a general operation here or composes one. |
| §4 rule 1 | No command event: FileStorage writes no Timeline fact. |
| §4 rule 2 | No handoff: the bytes and the key stay with FileStorage. |
| §4 rule 3 | No before callback and no commit-lock work; no caller code runs under a file key. |
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

The service is marked on the owner type; the wire name stays `fsHost`. The `FsHost` wrapper type no longer exists. `caller: &Ctx` in the signatures below is rutis's caller parameter: the dispatch fills it from the calling row, it is not a request field, and no caller sends or names a row.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

pub type RootId = String;
pub type ReadId = String;
pub type WriteId = String;

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteHandle {
    pub write_id: WriteId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Expected {
    Absent,
    Digest { digest: String },
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
    #[error("{0} is not the expected version")]
    Conflict(String),
    #[error("the replaced copy already exists: {0}")]
    BackupExists(String),
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

    pub async fn open_write(
        &self,
        caller: &Ctx,
        path: String,
        permissions: Option<Permissions>,
        cancelled: CancelledFn,
    ) -> Result<WriteHandle, FileError> {
        // ...
    }

    pub async fn write_chunk(
        &self,
        caller: &Ctx,
        write_id: WriteId,
        bytes: Bytes,
        cancelled: CancelledFn,
    ) -> Result<(), FileError> {
        // ...
    }

    pub async fn commit_write(
        &self,
        caller: &Ctx,
        write_id: WriteId,
        cancelled: CancelledFn,
    ) -> Result<u64, FileError> {
        // ...
    }

    pub async fn abort_write(&self, caller: &Ctx, write_id: WriteId) -> Result<(), FileError> {
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
        expected: Expected,
        content: Bytes,
        permissions: Option<Permissions>,
        keep_replaced_as: Option<String>,
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

`CancelledFn` is the crate's existing cancellation argument ([`host::CancelledFn`](../../../crates/dim-agent-capabilities/src/host.rs)). `Bytes` crosses as base64 text; `Name` as a string or a native-bytes object; `RootId`, `ReadId`, `WriteId` and sizes as strings and numbers. No request or reply contains a closure; `CancelledFn` is the binding's cancellation argument and `caller: &Ctx` is the dispatch's caller argument, neither is a request field.
