# FileStorage

Scope: process. Issue: #2016.

FileStorage owns byte storage under explicitly authorized roots. This document is the owner's target contract; the current implementation is described in the [FileStorage plugin page](../../architecture/rust-agent/plugins/FileStorage.md). PluginFiles is a separate owner with its own contract and is not covered here; its calls appear in the delivery tables in the [delivery comment on #2016](https://github.com/arcships/dim/issues/2016#issuecomment-7090617699).

## Use and scope: responsibility split

FileStorage owns:

- **Roots.** The process's authorized directories and their open handles. The host registers a root through the provider when it assembles the process and when it creates a Session; the public face exposes neither registration nor root lookup. A caller receives absolute paths from the host value that names its root.
- **Generations.** One service instance admits calls, owns the descriptors it opened and releases them on close.
- **Exclusion.** Each file's lock key beside the file. A replacement, removal or update holds the key; another process holds the same key for the same file under every case and Unicode spelling the filesystem equates. FileStorage waits for the key until the caller cancels or the owner closes.
- **Atomic replacement.** A temporary file in the owner's namespace, flushed and renamed over the target, with the durability answer.
- **Reads.** A whole document up to 16 MiB in one call, or one 64 KiB page at an offset; both are stateless, so no read id crosses.
- **Path validation.** Absolute paths inside a root, one validated component per open, no link followed, the owner's own resource names excluded.

Callers keep:

- Document formats and versions: settings documents, `auth.json`, the plugin envelope, JSONL records.
- Which root and file a business uses, and the file names.
- What a business conflict means and when to retry it.
- Permission and approval: FileStorage never decides whether a call may run.
- Workspace, system and worktree facts: FileAccess owns them; FileStorage answers only whether an absolute path is inside a root.
- Files a plugin creates and manages itself under a root, such as SQLite's database, the model-log recorder's JSONL and the UI extension cache.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one document: settings, accounts, plugin records, a project's hooks file | Yes: `read` without an offset reads the whole document and answers none when absent | Business composes |
| Read a large file in pages | No: the owner bounds one reply to a page and reports the end, so the caller never holds an id or a page size of its own | `read` with `offset` and `length` |
| Replace a document's bytes atomically | Yes: `write` whose source yields the whole content once | Business composes |
| Write content that arrives in pieces, including a stream an adapter produces | No: holding it whole is unbounded; the temporary file and the key must stay inside the owner | `write` with a source |
| Read, decide and write back while other processes may replace the file | No: between a read and a write another writer replaces the file, so only the owner can hold the file across the caller's decision | `update` with a change |
| Keep the replaced bytes before a destructive rewrite | Yes: read the current bytes, `write` a sibling copy, then `update` whose change refuses when the bytes moved | Business composes; DimOAuth compares inside the change and retries |
| Bring a host file the caller named into a root | No: the source is outside the roots and must be bounded and land atomically | `import` |
| Keep a document's file owner-only from creation (settings, accounts) | No: the mode is a file attribute the owner applies at creation and replacement | `permissions` on `write`, `import` and `update` |
| Write a Session file that `write` and `edit` may not modify | No: the read-only attribute is a file attribute the owner applies | `permissions` |
| Bound a host file import by the business limit (a Session attachment) | No: the caller's limit must stop the copy before it lands | `import` with `max_bytes` |
| Make a work directory tree and learn its absolute path | No: per-component link and alias validation is the owner's knowledge | `create_directory` |
| List one level and remove by a listed native name | No: the name's encoding and the single-component rule are the owner's knowledge | `list`, `remove` |
| Make a runtime-discovered directory addressable | Yes: the host registers the root when it assembles or creates the Session | Business side: host registration, not this face |
| Hand a root's directory to another module: a process working directory, a path quoted in history, a log directory | Yes: the host value that names the root carries the path | Business side: host value, not this face |
| Learn whether an optional file exists (a project hooks file, a Settings source) | Yes: `read` answers none; `list` answers the parent's entries | Business composes |
| Decode or encode a document, choose its name and its root | Yes: formats and business paths stay with callers | Business |
| Append records to a log it owns (the model-log recorder) | Yes: the recorder owns the file; the host hands it the directory | Business; the file is the recorder's, as SQLite's database is |
| Write a UI extension cache and then publish it | Yes: the cache belongs to UiExtensions; FileStorage reads it only on request | Business |
| Decide whether a read or write is allowed | Yes: Permission and Approval decide | Business |

An operation or parameter exists only where its row says No.

## Caller example

Reading one document, and one page of a file too large to hold:

```rust
// Whole document: none when absent, too_large above 16 MiB.
let document = storage.read(path.clone(), None, None, cancelled.clone()).await?;
// One 64 KiB page: eof says the page reached the end; an offset past the end reads empty.
let page = storage.read(path, Some(offset), Some(PAGE), cancelled).await?;
```

Settings' compare-and-set: the change runs under the owner's file key, sees the current content and returns the next content or a business refusal.

```rust
storage.update(
    path,
    Arc::new(move |current: Option<Bytes>| {
        Box::pin(async move {
            let current = decode(current.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
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
let size = storage.write(path, once(bytes), Some(Permissions::PrivateReadOnly), cancelled.clone()).await?;
// A host file the caller named, without reading it first.
let size = storage.import(path, source, Some(MAX_FILE_BYTES), Some(Permissions::PrivateReadOnly), cancelled).await?;
```

DimOAuth's reset: copy the current file, then rewrite it under the owner's key. The change refuses when the bytes are no longer the ones that were copied, and the caller retries the whole step.

```rust
let current = storage.read(auth_path.clone(), None, None, cancelled.clone()).await?
    .ok_or(OAuthError::UnsupportedSchema)?;
// The backup is a business copy; the rewrite starts only after the copy is durable.
storage.write(backup_path, once(current.clone()), Some(Permissions::PrivateReadOnly), cancelled.clone()).await?;
storage.update(
    auth_path,
    Arc::new(move |stored: Option<Bytes>| {
        Box::pin(async move {
            if stored.as_ref() != Some(&current) {
                return Err(Rejected::new("conflict", "the account file changed after it was copied"));
            }
            let mut document = parse(stored.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
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

UiResources' pages of a published file, with no id to release:

```rust
match storage.read(request.path.clone(), Some(offset), Some(PAGE), cancelled.clone()).await {
    Ok(Some(page)) => page,                       // data, eof
    Err(FileError::Invalid { .. }) => return Err(ResourceError::service("resource_unavailable")),
    other => return other.map_err(...),
}
```

## Callers that hold work across an external wait

The public face has no lease. Three callers need exclusion across work that cannot run under the file key; each composes it from `update`, and the owner keeps the guarantee that one `update` is exclusive across processes.

**Managed OAuth.** The HTTP refresh runs outside the key. The refreshed token is written with one `update`: the change compares the stored record with the record read before the refresh. Equal means no other process rotated the token, so the change writes the new token; different means another process won, so the change returns the stored content unchanged and the caller adopts that value. Two concurrent refreshes with the same refresh token both complete; the first `update` wins and the second adopts the stored record, so the stored refresh token is always the one a completed refresh last wrote.

**Migration snapshot run.** The run claim is a file whose content carries the run id, the owning process id and its start time. The claim is created by one `update` whose change writes the content only when the file is absent and otherwise refuses with `held`. A later run that finds a claim checks the recorded process: alive means the claim stands; gone means the change replaces the stale claim inside the same `update`. A process that exits leaves the claim behind, and the next run reclaims it by that liveness check; no key is held while a run copies files.

**PluginFiles `withLock`.** A multi-step change that is a pure transform of one record is one `update` over that record, and the cross-process guarantee is exactly "one `update` is exclusive". A change with side effects outside the record does them before the `update` and retries when the change refuses with `conflict`. PluginFiles serializes its callers inside itself where a caller expects its several operations to run alone; FileStorage provides no multi-call session.

## Own contract

### Dependencies

None. FileStorage injects no service and reads no other owner's state. The host registers roots and hands their paths to callers as values; the service takes absolute paths. The provider is chosen by deployment ([resource faces §1](../resource-faces.md#1-scope)): whether a call reaches a local or a mounted instance is not a property of this name.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `read` | Read a whole document, or one page of a file | `{path, offset?, length?}` | `{data, eof}` or none | Before the reply; nothing changes | `invalid`, `too_large`, `closed`, `cancelled` |
| `write` | Replace a file's bytes atomically from a source the owner pulls to its end | `{path, permissions?}` and the `source` callback | `{size}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `import` | Copy a regular host file into a root atomically | `{path, source, max_bytes?, permissions?}` | `{size}` | Before the replacement; the temporary file is removed | `invalid`, `too_large`, `missing_parent`, `unsynced`, `closed`, `cancelled` |
| `update` | Change a file's content while the owner holds the file across the caller's decision | `{path, permissions?}` and the `change` callback | `{applied}` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `too_large`, `rejected`, `unsynced`, `closed`, `cancelled` |
| `create_directory` | Make the path an existing directory tree | `{path}` | `{path}` | Between levels; created ancestors stay, a retry is safe | `invalid`, `unsynced`, `closed`, `cancelled` |
| `list` | One level of a directory | `{path}` | `{entries}` or none when the directory is absent | Before the reply | `invalid`, `closed`, `cancelled` |
| `remove` | Make a file or a directory tree absent; an absent entry counts as removed | `{path}` or `{parent, name}` | `{}` | Before the entry is unlinked; a tree can be partly removed, a retry is safe | `invalid`, `unsynced`, `closed`, `cancelled` |

`read` without `offset` and `length` reads the whole document up to 16 MiB and answers `too_large` above it; with both, it reads one page of at most 64 KiB (`length` above the page is clamped) and `eof` is true when the page reaches the end; one of the two alone is `invalid`. An absent file answers none.

`write`'s `source` and `update`'s `change` are direct method parameters, not request fields. `source` is called repeatedly until it returns none: each call yields the next chunk of at most the bridge frame, or a `Rejected` error that ends the write and leaves the target unchanged. `change` is called once while the file's key is held, with the current content (at most 16 MiB) or none when the file is absent, and returns the next content or a `Rejected` error; `applied` is false when that content equals the current content, so a repeated write is idempotent. `change` must only compute: waiting on another owner, on network I/O or on FileStorage itself inside `change` holds the file for that whole wait. §7 allows a callback by reference, and the change is never called inside the Timeline's commit lock.

`permissions` is absent for "keep the existing mode; a new file gets the owner's default": `private` is owner-only (0600), `privateReadOnly` is owner-only and read-only (0400), the attribute Session files use. `max_bytes` is absent for the owner's limit. A listed name is passed back to `remove` unchanged. FileStorage waits for a file's key until the caller cancels or the owner closes; there is no fixed bound, so a call that must not wait ends with its cancellation.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a relative path, a destination outside every root, a reserved name, a name that is not one component, a directory where a file is required, a file where a directory is required, a link where a directory is required, one of `offset` and `length` without the other; for `import`, a source that is relative, missing, not a regular file or a link |
| `missing_parent` | Create the parent (`create_directory`) or fix the path |
| `too_large` | Read it in pages with `offset` and `length`, or treat it as not a document |
| `rejected` | The `change` or `source` refused with its own `reason` and `message`; act on `reason`, for example re-read and retry the business step on `conflict` |
| `unsynced` | The bytes are in place but durability is unconfirmed; a read sees them and a repeated call confirms the content; do not assume the earlier call failed |
| `cancelled` | `read`, `write` and `update` changed nothing before the replacement; `create_directory` and `remove` follow their cancellation columns: created ancestors stay, a tree can be partly removed |
| `closed` | This generation is unloading; call the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `read` | Open the parent and the file without following links, read the whole document or the page, release | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | Descriptor by FileStorage |
| `write` | Parent handle, file key, temporary file, pull the source to its end, flush, rename, sync parent, release key | The pull stops, the temporary file is removed, the target is unchanged, `cancelled` | The source callback is revoked; the pull fails; the temporary file is removed, `cancelled` | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Temporary file and key by FileStorage; the source's future by the calling row |
| `update` | Parent handle, file key, read the current content within the document limit, call `change`, write the temporary file, flush, rename, sync parent, release key | Before the rename: temporary file removed, target unchanged, key released, `cancelled`. After the rename: success or `unsynced`, the write is not undone | The change callback is revoked; the call ends `cancelled`; the same release applies | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Key and temporary file by FileStorage; the change's future by the calling row |
| `import` | Open the source without following links, require a regular file within the limit, copy into a temporary file, rename, sync parent | The temporary file is removed, the target is unchanged, the source closes | The call ends `cancelled` | Pending calls end `closed` | Source descriptor and temporary file by FileStorage |
| `remove` | Parent handle, the entry's key, remove depth first without following links, sync the parent | Before the unlink: nothing removed; a tree can be partly removed, a retry is safe | The call ends `cancelled` | Pending waits end `closed`; the generation releases keys | Entry removal by FileStorage |
| `create_directory` | One level at a time below the held parent, mode 0700, sync the parent | Between levels; created ancestors stay | The call ends `cancelled` | Pending calls end `closed` | Created directories by FileStorage |
| `list` | Open the directory, read one level, sort by name | Before the reply | The call ends `cancelled` | Pending calls end `closed` | Directory handle by FileStorage |

## Exceptions with reasons

- `import` reads a host path outside the roots: the caller names a file the user or model chose (an attachment); the owner checks that the source is a regular file, does not follow a link and enforces the byte limit, while the root constrains only the destination.
- The account marker `auth.json.lock` stays in its published TypeScript-compatible form: the account file is shared with the TypeScript subscriber, and both sides must lock the same file under the same marker protocol.
- `Rejected` carries only `reason` and `message`: a callback error must cross languages, so it comes back as data; `From<CallError>` recovers the wire error's own code into `reason`, and the caller keeps its business error type inside its own callback.
- A file another plugin owns under a root — SQLite's database, the model-log recorder's JSONL, the UI extension cache — is not FileStorage's data; FileStorage does not track ownership and cannot refuse a `remove` of it. The convention that a plugin removes only its own files is the caller's, and a root is not an ownership boundary.
- The host registers roots and hands their paths as values; a root path is not a grant, because Permission and Approval decide what an agent action may do and FileStorage's roots only say which directories are addressable.

## Client-visible changes

None. This contract changes the plugin-facing `fsHost` surface only. Client protocol error codes, notification names, Session file offset paging, the import reply's `path`, model-visible text, and disconnect and progress behavior are unchanged.

## Supersedes

- `docs/design/resource-faces.md`: the `fsHost` row of §1, the storage family and error table of §4, and the `fileStorage` row and FileStorage caller rows of §6. The tool family (`resolve`, `read`, `write`, `edit`, `glob`, `grep`, `mentions`) is the file-tools owner's and is not part of `fsHost`; [#1297](https://github.com/arcships/dim/issues/1297) tracks that design.
- `docs/design/contracts/resource-owner-operations.md`: the `fileStorage` row.
- `docs/design/service-faces.md`: the `fileStorage` row of §3.4 and the `fileStorage` sentence in the paragraph after the table.
- `docs/design/appserver/session-files.md`: the FileStorage operation names in §3, §5, §7, §9 and §10. SessionStore's own rows, paging and admission are unchanged.
- `docs/design/p6-p7-bindings.md`: the paragraph that says `FileStorage` has `register_root` and `root_path`; root registration moved to the host and root lookup is not on the face.
- `docs/architecture/rust-agent/plugins/FileStorage.md`: the current implementation page; it describes the code, not this contract.

## Verification

Flow tests on the owner:

- `read` answers none for an absent file, `too_large` above 16 MiB for a whole read, one page of at most 64 KiB with `eof` at the end, and an empty page past the end.
- `read` stays stateless: no id is issued, and a page read after a replace sees the replaced content.
- `write` pulls a multi-chunk source, commits atomically, removes the temporary file when the source fails or is revoked, and leaves the target when cancelled.
- `update` calls the change under the key; a change that returns `Rejected` leaves the file unchanged; an identical result reports `applied: false`; a cancel before the rename removes the temporary file; a failed parent sync reports `unsynced`.
- `import` over `max_bytes` copies nothing; a missing, non-regular or linked source is `invalid`; a destination outside every root is `invalid`.
- `remove` of a tree with a file another writer holds waits, and cancelling ends the wait with `cancelled`; a retry after a partial removal finishes; a link is not followed.
- A call in flight when the generation closes ends `closed`.
- The host registration flow test: a root registered at assembly and one registered for a Session resolve to the intended directories after a provider reload.

Material uncertainty: a change that waits on another owner or on I/O holds the file's key for that wait. The contract forbids it; review checks each caller's change.

## Rule check

The rules are [design rules §7](https://github.com/arcships/dim/pull/2029) as proposed in #2029, and §1–§6 of `docs/architecture/rust-agent/principles.md`. §1–§6 already exist in the repository; §7 is in #2029 until it merges.

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
| §4 rule 8 | No infrastructure change and no requirement to a mechanism owner. |
| §4 rule 9 | No Timeline fact. |
| §7 callbacks | The write source and the update change cross by reference as §7 allows; no object, handle or id crosses. |
| §7 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Process scope: roots, descriptors, keys and generations outlive Sessions and are shared across them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; no adapter or facade exists beside it. |
| ADR 002 D3 | Dependencies point from extensions to the core; FileStorage depends on nothing and is a process service they may depend on. |
| ADR 002 D4 | Roots are structural: the host registers them; no business setting is involved. |
| ADR 002 D5 | The contract is language-neutral: operations, data and callbacks; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

None.

## Binding appendix

The service is marked on the owner type; the wire name stays `fsHost`. The `FsHost` wrapper type no longer exists. `CancelledFn` is the binding's cancellation argument, not a request field. Error codes are the snake_case `code` values of the error table.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use std::sync::Arc;

pub const DOCUMENT_LIMIT: u64 = 16 * 1024 * 1024;
pub const PAGE_LIMIT: u32 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Bytes(#[serde(with = "base64_bytes")] pub Vec<u8>); // a small local serde module; crosses as base64 text

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permissions {
    #[default]
    Preserve,
    Private,
    PrivateReadOnly,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadReply {
    pub data: Bytes,
    pub eof: bool,
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

/// A listed name is passed back unchanged: a string, or the native bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Name {
    Text(String),
    Unix { #[serde(rename = "Unix")] unix: Vec<u8> },
    Windows { #[serde(rename = "Windows")] windows: Vec<u16> },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RemoveTarget {
    Path { path: String },
    Entry { parent: String, name: Name },
}

/// The caller's own refusal or failure from a source or a change, carried unchanged.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{reason}: {message}")]
pub struct Rejected {
    pub reason: String,
    pub message: String,
}

impl Rejected {
    pub fn new(reason: &str, message: &str) -> Self {
        Self { reason: reason.to_owned(), message: message.to_owned() }
    }
}

/// A failed callback arrives as the wire's error; its own code becomes `reason`.
impl From<rutis::host::CallError> for Rejected {
    fn from(error: rutis::host::CallError) -> Self {
        let (message, raw) = error.into_raw();
        let reason = raw
            .and_then(|raw| raw.downcast::<rutis_bridge::session::Error>().ok())
            .and_then(|error| error.code().map(str::to_owned))
            .unwrap_or_else(|| "failed".to_owned());
        Self { reason, message }
    }
}

/// The next chunk of a write, or none at the end.
pub type SourceFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Option<Bytes>, Rejected>> + Send + Sync>;

/// The current content (none when absent); the next content or a refusal.
pub type ChangeFn =
    Arc<dyn Fn(Option<Bytes>) -> BoxFuture<'static, Result<Bytes, Rejected>> + Send + Sync>;

#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FileError {
    #[error("{message}")]
    Invalid {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    #[error("{message}")]
    MissingParent { message: String, path: String },
    #[error("{message}")]
    TooLarge { message: String, path: String, size: u64 },
    #[error("{reason}: {message}")]
    Rejected { reason: String, message: String },
    #[error("{message}")]
    Unsynced { message: String, path: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    Closed { message: String },
}

impl PublicService for FileStorage {}

#[rutis::service(name = "fsHost")]
impl FileStorage {
    pub async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        length: Option<u32>,
        cancelled: CancelledFn,
    ) -> Result<Option<ReadReply>, FileError> {
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

`CancelledFn` is the crate's existing cancellation argument ([`host::CancelledFn`](../../../crates/dim-agent-capabilities/src/host.rs)). `Bytes` crosses as base64 text; `Name` as a string or the native `{Unix: …}`/`{Windows: …}` object; sizes as numbers. Requests carry callbacks by reference only where the operation needs the caller's own step — the write source and the update change; every other argument and reply is data.
