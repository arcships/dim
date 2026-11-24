# SessionFiles

Scope: process. Issue: #2018.

SessionFiles owns the bytes of Session files and the ownership rows that make a path or a file id belong to a Session. This document is the owner's target contract. The current implementation is described in [Session files](../appserver/session-files.md); the interface there is superseded by this contract. The engine's row operations this owner uses are private to the storage crate; the Session store uses this owner when a delete releases files.

## Use and scope: responsibility split

SessionFiles owns:

- **The file namespace.** One file per `fileId` under `<db>.session-files/<fileId>/<name>`, written through FileStorage.
- **The write generation.** One generation per store generation. A write reserves its name, the bytes land, and the ownership row commits only in the same generation that reserved it.
- **The sweep.** Removes files no row owns, rows pending under a dead generation, and directory entries with no row.
- **Ownership rows.** Which Session owns a Session file and which Session owns a registered output path.
- **Path mapping.** An output path is a path; a Session file path is a file id inside the owner's directory. The owner resolves between them.

Callers keep:

- **The purpose of the file.** An attachment, an image sent to a model, a tool result, an event payload, a command output path.
- **The name** the caller gives a stored file, within the owner's component rule.
- **The output roots.** WorkspaceTools owns `command-output/<session>/…`; HostPlatform owns `.dim/exec/<session>/<branch>/…`. The caller decides which root a path belongs to and passes a resolved absolute path; the owner records it.
- **The model's content rules.** Image budgets, request materialization and what reaches the model stay with the model provider.
- **The tool result.** What a registration means for the user stays with the tool.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Store produced bytes under a new file id for a Session; an event body names the path a read tool opens | No: the reservation, the file write and the ownership commit are one ordered protocol, and the path is the owner's layout | `put` |
| Import a host file the user or model named as a Session file | No: the source is outside the file namespace and the size limit must stop the copy before it lands | `import` |
| Replace an existing Session file's bytes under the same id and path; an MCP result file is rewritten | No: the id, the path and the ownership row are the owner's; a replace must keep the read tool's path valid | `write` |
| Read a Session file in bounded pages for a model request, an attachment read or a web download | No: the caller cannot read the owner's directory or the file id's row | `read` |
| Record a command output path as owned by the Session | No: ownership is the owner's row | `register` |
| Answer whether a path or file id belongs to the Session | No: the answer composes two ownership tables and the file directory mapping | `owns` |
| Remove an MCP result file and its ownership row | No: the directory entry and the row are the owner's | `remove` |
| List every path the Session owns, for deletion or for a grant | No: the caller has no row access | `ownedPaths` |

An operation exists only where its row says No.

## Caller example

WorkspaceTools registers a command output path it owns the root of.

```rust
files.register(Register { session_id, path }).await?;
```

Attachments reads one 64 KiB page and stops at the end of the file.

```rust
let page = files.read(ReadFile { session_id, file_id, offset }, cancelled.clone()).await?;
// `page.data` is the next page in base64; `page.eof` is true exactly when the page is empty.
if page.eof { /* the end of the file */ }
```

Attachments imports a host file the user chose; the reply carries the path the tool result names.

```rust
let stored = files.import(Import { session_id, name, source }, cancelled).await?;
```

## Own contract

### Dependencies

`fsHost` for `root_path`, file creation, atomic replacement, import, page reads, listing and removal; the operations used are the ones in [FileStorage's contract](https://github.com/arcships/dim/pull/2080). The owner joins the store directory's root path from `root_path` with `<db>.session-files` and passes absolute paths under it; the host registers the native store directory as a root. The engine's file-ownership rows are private to the storage crate and are not a service. The owner does not depend on `store`: the store depends on this owner when a delete releases files.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `put` | Store bytes under a new file id and commit ownership | `{sessionId, name, content}`, `content` is base64 | `{fileId, path, bytes}` | While the bytes land; a cancelled put leaves no owned row | `invalid`, `too_large`, `not_found`, `node_unavailable`, `unsynced`, `missing_parent`, `cancelled`, `closed`, `storage` |
| `import` | Copy a regular host file into the Session namespace and commit ownership | `{sessionId, name, source}` | `{fileId, path, bytes}` | Before the replacement; a cancelled import leaves no owned row | `invalid`, `too_large`, `not_found`, `node_unavailable`, `unsynced`, `missing_parent`, `cancelled`, `closed`, `storage` |
| `write` | Replace one Session file's bytes under the same id and path | `{sessionId, fileId, content}`, `content` is base64 | `{fileId, path, bytes}` | Before the replacement; the old bytes stay | `too_large`, `not_found`, `node_unavailable`, `unsynced`, `missing_parent`, `cancelled`, `closed`, `storage` |
| `read` | One page of a Session file, at most 64 KiB | `{sessionId, fileId, offset}` | `{data, eof}`, `data` is base64 and `eof` is true exactly when it is empty | While the page is read; nothing changes | `not_found`, `node_unavailable`, `cancelled`, `closed`, `storage` |
| `register` | Record a caller-owned output path for the Session | `{sessionId, path}` | `{}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `owns` | Whether a path or file id belongs to the Session | `{sessionId, path}` | `{owned}` | While the path is resolved | `node_unavailable`, `cancelled`, `closed`, `storage` |
| `remove` | Remove one Session file and its ownership row; an absent or foreign file answers success | `{sessionId, fileId}` | `{}` | None; the owner completes the removal during close | `node_unavailable`, `cancelled`, `closed`, `storage` |
| `releaseSession` | Delete every ownership row of a deleted Session and schedule the sweep; a repeated call succeeds | `{sessionId}` | `{}` | None; the owner completes the release | `closed`, `storage` |
| `ownedPaths` | Every path the Session owns | `{sessionId}` | `{paths}` | None | `closed`, `storage` |

`path` is absolute on the Session's resource node; `bytes` is the stored length. `offset` at the file size answers an empty `data` with `eof`; an offset above the size is clamped to the size before the page read, because FileStorage refuses an offset above it with `invalid`. The rule is stated once in both documents: at `size` an empty page with `eof`, above `size` `invalid` at FileStorage; this owner clamps so its own callers keep the empty page their client contract states. A `fileId` outside the Session, or a name that is not one component, is `not_found` for reads and writes and `invalid` for a name `put` or `import` receives.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a name that is not one component, a relative or non-UTF-8 path, an import source that is not a regular file |
| `too_large` | Treat the file as not storable; store a smaller representation or refuse the attachment |
| `not_found` | The Session has no such file, the file belongs to another Session, or the Session no longer exists; the caller refuses the request |
| `node_unavailable` | The Session's resource node cannot be reached; retry later or report the node as unavailable |
| `unsynced` | The bytes are in place but durability is unconfirmed; do not treat the write as failed or as committed. Read the file to confirm, then retry the same write; the replacement is idempotent |
| `missing_parent` | The target path's parent is missing; this is the caller's path error. Create the directory or fix the path, then retry |
| `cancelled` | The reservation and any copy are left to the sweep; no ownership row was committed, and the caller stops |
| `closed` | This generation is unloading; call the replacement generation |
| `storage` | The store or the file system failed; the caller reports the failure and keeps its own copy |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `put` | Reserve the file under the generation, write the bytes through FileStorage, commit the ownership row while the generation and the Session still exist | The reservation and the written bytes are abandoned; the sweep removes them; no owned row | The write completes under the owner's task, or the reservation expires with the generation; the sweep removes unowned bytes | Admission stops; admitted writes finish; a new generation sweeps the old one's pending rows | FileSystem by FileStorage; rows and sweep by SessionFiles |
| `import` | Open the source, check size, copy through FileStorage into a temporary file, commit ownership | The temporary file is removed; no owned row | The copy completes or is abandoned; the sweep removes what no row owns | As `put` | Source and temporary file by FileStorage; rows by SessionFiles |
| `write` | Resolve the file id, write the new bytes through FileStorage under the same path, keep the row | Before the replacement: the old bytes stay | The write completes or is abandoned | As `put` | File and path by FileStorage; row by SessionFiles |
| `read` | Resolve the file id, read one page at the offset; an offset at or past the end is an empty page | The page stops; nothing changes | The read stops | Pending reads end `closed` | FileStorage |
| `remove` | Resolve the file id; an absent or foreign id answers success; remove the directory entry through FileStorage, delete the row | Before the unlink: nothing removed, `cancelled`; a tree can be partly removed and a retry is safe | The call ends | A new call is refused `closed`; an admitted removal finishes | File and row by SessionFiles |
| `releaseSession` | Delete every ownership row of the Session, schedule the sweep; the bytes are deleted outside the transaction | None | The release continues under the owner's task | A new call is refused `closed`; an admitted release finishes | Rows by SessionFiles; bytes by the sweep |
| `register` | Check the Session, record the path the caller resolved | None | The row stays; a later Session delete removes it | A new call is refused `closed`; rows stay for the next generation | SessionFiles |
| `owns` | Resolve the path against the owner's directory, check the output row, else the file id row | The resolution stops | Nothing to release | Pending calls end `closed` | SessionFiles |
| `ownedPaths` | Read both ownership rows, map file ids to the directory | None | Nothing to release | A new call is refused `closed`; rows stay | SessionFiles |

A Session delete releases this owner's rows before the sweep runs; the sweep deletes bytes no row owns. Files another Session owns are never removed by one Session's delete.

## Exceptions with reasons

- The file directory is below the database directory: a Session's files are removed with the store that owns their rows, and the delete's last-owner guard is scoped to one database (ADR 013).
- The generation is the store's generation: a stale writer cannot commit into a Session that a newer store generation created.
- `read` pages by offset instead of opening a read id: the callers (Attachments' `attachment.read`, the web download, image materialization) already page by offset, and a read id would put its release on every error path.
- `register` records a path the caller resolved and owns the root of: WorkspaceTools and HostPlatform have two different output roots, and the owner cannot decide which root a path belongs to without depending on both.
- `write` and `remove` serve MCP result files ([#2101](https://github.com/arcships/dim/pull/2101)): a result file is rewritten as the child reports progress and removed when the child's result is discarded.
- The ownership rows live in the engine's database but belong to this owner: the engine executes the transaction, SessionFiles decides when it commits.

## Client-visible changes

None. `read` keeps the offset page and the 64 KiB bound; `put` and `import` keep `{fileId, path, bytes}`; the error codes and the `attachment.read` reply are unchanged. `write` and `remove` are new plugin operations, not client protocol methods.

## Supersedes

- [Session files §9.1](../appserver/session-files.md#91-language-neutral-adr-002-d5): the operation table, the paging note, the register root rule and the paragraph that says SessionStore offers no operation that changes or removes one file.
- [Service faces §3.2](../service-faces.md): the `sessionFiles` row.
- [Resource faces §4.6](../resource-faces.md): the Session files paragraph.
- [Overview](../overview.md): the SessionStore row's Session files clause.

## Verification

Flow tests on the owner:

- `put` reserves, writes and commits; a cancelled put leaves no owned row and the sweep removes the bytes; a second put with the same name gets a new file id; the reply names the path and the stored length.
- `import` refuses a directory, a non-regular file and a source over the limit without landing anything; the reply names the path.
- `read` pages by offset; the last page has `eof`; an offset past the end is an empty page; a foreign file id answers `not_found`.
- `write` keeps the file id and path and replaces the bytes; an unknown id answers `not_found`; the old bytes survive a cancelled write.
- `remove` answers success for an absent or foreign file, deletes the file and the row for an owned one.
- `register` records a path without judging its root; a missing Session answers `not_found`; `owns` answers true for a registered output and for a Session file path, and false for another Session's file.
- A Session delete releases the rows; the sweep removes the bytes; a file still owned by another Session survives.
- A generation close stops admission; a new generation sweeps pending rows and directory entries without rows.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | SessionFiles is one process part. It writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: the bytes and rows outlive Sessions; callers are process or Session parts. |
| §3 orthogonality | The file namespace and the ownership rows are this owner's; other owners read them through the operations. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No Session or model policy. |
| §4 rule 5 | No batches. |
| §4 rule 6 | File content reaches the model only through the model provider's request; SessionFiles returns bytes to the owner that asked. |
| §4 rule 7 | Callers use these operations; no caller reads a generation or a directory entry. |
| §4 rule 8 | No infrastructure change; requirements below. |
| §4 rule 9 | No Timeline fact. |
| §7.2 ids and values | File ids, paths and bytes cross as data; a `fileId` is the resource id; pages are offset reads. |
| §7.2 one public face | The service is marked on the file owner's type under the existing `sessionFiles` name; the `SessionFiles` wrapper type and the `SessionFilesService` trait do not exist beside it. |
| §7.2 caller mechanisms | The reservation, the generation and the sweep stay inside the owner; callers handle the business errors only. |
| ADR 002 D1 | Process scope: the namespace and the rows outlive Sessions. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The owner depends on `fsHost`; callers depend on `sessionFiles`. |
| ADR 002 D4 | The 16 MiB limit is the owner's; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **FileStorage (#2016): the operations this owner calls.** `root_path`, `read` with `{offset, length}` and `{data, eof, size}`, `write` with its `source` and `privateReadOnly`, `import` with `max_bytes` and `privateReadOnly`, `create_directory`, `list` and `remove` exist in [FileStorage's contract](https://github.com/arcships/dim/pull/2080) at head `2a312945c`. The owner reads the store directory's root with `root_path`; the host registers the native store directory as a root, and a comment on #2016 records the dependency.
2. **Service type placement (#2015): a process owner in the app server.** The owner type is in `dim-app-server`, where the engine and the rows are; callers in plugin crates cannot depend on the app server. The request and reply types are plain data in `dim-agent-capabilities::host`, and callers outside the crate call the operations by the `sessionFiles` name. If the maintainers decide every process face must be annotated on a type plugin crates can name, the type moves to a shared crate; a comment on #2015 states the requirement.

## Binding appendix

The service is marked on the file owner's type; the wire name stays `sessionFiles`. The `SessionFiles` wrapper and `SessionFilesService` no longer exist. `CancelledFn` is the binding's cancellation argument; operations that do not wait take none.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;
use std::sync::Arc;

/// The wire error: one variant per code; `code` is the variant name in snake_case.
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SessionFileError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    TooLarge { message: String },
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    NodeUnavailable { message: String },
    #[error("{message}")]
    Unsynced { message: String },
    #[error("{message}")]
    MissingParent { message: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the file service is unloading")]
    Closed,
    #[error("{message}")]
    Storage { message: String },
}

impl PublicService for SessionFiles {}

#[rutis::service(name = "sessionFiles")]
impl SessionFiles {
    pub async fn put(
        &self,
        caller: &Ctx,
        request: Put,
        cancelled: CancelledFn,
    ) -> Result<StoredFile, SessionFileError> {
        // ...
    }

    pub async fn import(
        &self,
        caller: &Ctx,
        request: Import,
        cancelled: CancelledFn,
    ) -> Result<StoredFile, SessionFileError> {
        // ...
    }

    pub async fn write(
        &self,
        caller: &Ctx,
        request: WriteFile,
        cancelled: CancelledFn,
    ) -> Result<StoredFile, SessionFileError> {
        // ...
    }

    pub async fn read(
        &self,
        caller: &Ctx,
        request: ReadFile,
        cancelled: CancelledFn,
    ) -> Result<Page, SessionFileError> {
        // ...
    }

    pub async fn register(
        &self,
        caller: &Ctx,
        request: Register,
    ) -> Result<(), SessionFileError> {
        // ...
    }

    pub async fn owns(
        &self,
        caller: &Ctx,
        request: Owns,
        cancelled: CancelledFn,
    ) -> Result<Owned, SessionFileError> {
        // ...
    }

    pub async fn remove(
        &self,
        caller: &Ctx,
        request: RemoveFile,
    ) -> Result<(), SessionFileError> {
        // ...
    }

    pub async fn owned_paths(
        &self,
        caller: &Ctx,
        request: OfSession,
    ) -> Result<Paths, SessionFileError> {
        // ...
    }
}
```

`SessionFiles` is the current `files::SessionFiles` owner type; the wrapper named `SessionFiles` in `dim-agent-capabilities` is deleted, not renamed. Every operation takes one request object, one reply object and the caller's `Ctx`; only the operations that wait take `CancelledFn`.
