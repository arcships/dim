# SessionFiles

Scope: process. Issue: #2018.

SessionFiles owns the bytes of Session files and the ownership rows that make a path or a file id belong to a Session. This document is the owner's target contract. The current implementation is described in [Session files](../appserver/session-files.md). The engine's row operations this owner uses are private to the storage crate; the Session store uses this owner when a delete releases files.

## Use and scope: responsibility split

SessionFiles owns:

- **The file namespace.** One immutable file per `fileId` under `<db>.session-files/<fileId>/<name>`, written through FileStorage.
- **The write generation.** One generation per store generation. A write reserves its name, the bytes land, and the ownership row commits only in the same generation that reserved it.
- **The sweep.** Removes files no row owns, rows pending under a dead generation, and directory entries with no row.
- **Ownership rows.** Which Session owns a Session file and which Session owns a registered output path.
- **Path mapping.** An output path is a path; a Session file path is a file id inside the owner's directory. The owner resolves between them.

Callers keep:

- **The purpose of the file.** An attachment, an image sent to a model, a tool result, an event payload, a command output path.
- **The name** the caller gives a stored file, within the owner's component rule.
- **The model's content rules.** Image budgets, request materialization and what reaches the model stay with the model provider.
- **The tool result.** What a registration means for the user stays with the tool.
- **The output directory.** WorkspaceTools owns the output path and its policy.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Store produced bytes under a new file id for a Session | No: the reservation, the file write and the ownership commit are one ordered protocol; a caller composing them would drive a half transaction | `put` |
| Import a host file the user or model named as a Session file | No: the source is outside the file namespace and the size limit must stop the copy before it lands | `import` |
| Read a Session file for a model request or a UI | No: the caller cannot read the owner's directory or the file id's row | `read` |
| Record a command output path as owned by the Session | No: ownership is the owner's row and the path must stay inside the Session's output directory | `register` |
| Answer whether a path or file id belongs to the Session | No: the answer composes two ownership tables and the file directory mapping | `owns` |
| List every path the Session owns, for deletion or for a grant | No: the caller has no row access | `ownedPaths` |

An operation exists only where its row says No.

## Caller example

WorkspaceTools registers a command output path.

```rust
files.register(Register { session_id, path }, cancelled).await?;
```

ImageUnderstanding and the model provider read a file for one request.

```rust
let stored = files.read(ReadFile { session_id, file_id }, cancelled.clone()).await?;
```

Attachments import a host file the user chose.

```rust
let stored = files.import(Import { session_id, name, source }, cancelled).await?;
```

## Own contract

### Dependencies

`fsHost` for the owner's directory, file creation, atomic replacement, import, listing and removal; the operations used are the ones in [FileStorage's contract](FileStorage.md). The engine's file-ownership rows are private to the storage crate and are not a service. The owner does not depend on `store`: the store depends on this owner when a delete releases files.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `put` | Store bytes under a new file id and commit ownership | `{sessionId, name, bytes}` | `{fileId, size}` | While the bytes land; a cancelled put leaves no owned row | `invalid`, `too_large`, `not_found`, `node_unavailable`, `cancelled`, `closed`, `storage` |
| `import` | Copy a regular host file into the Session namespace and commit ownership | `{sessionId, name, source}` | `{fileId, size}` | Before the replacement; a cancelled import leaves no owned row | `invalid`, `too_large`, `not_found`, `node_unavailable`, `cancelled`, `closed`, `storage` |
| `read` | Read one Session file whole, within the owner's limit | `{sessionId, fileId}` | `{bytes}` | While the file is read; nothing changes | `not_found`, `node_unavailable`, `cancelled`, `closed`, `storage` |
| `register` | Record an output path as owned by the Session | `{sessionId, path}` | `{}` | None | `invalid`, `not_found`, `storage` |
| `owns` | Whether a path or file id belongs to the Session | `{sessionId, path}` | `{owned}` | While the path is resolved | `node_unavailable`, `cancelled`, `storage` |
| `ownedPaths` | Every path the Session owns | `{sessionId}` | `{paths}` | None | `node_unavailable`, `storage` |

`bytes` is the file content as data; the owner's limit is 16 MiB. `read` answers the whole file within that limit: the limit is the owner's, so callers do not page. A `fileId` outside the Session, or a name that is not one component, is `invalid`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: a name that is not one component, a relative or non-file import source, a path outside the Session's output directory |
| `too_large` | Treat the file as not storable; store a smaller representation or refuse the attachment |
| `not_found` | The Session has no such file, or the Session no longer exists; the caller refuses the request |
| `node_unavailable` | The Session's resource node cannot be reached; retry later or report the node as unavailable |
| `cancelled` | Nothing was committed; the caller stops |
| `closed` | This generation is unloading; call the replacement generation |
| `storage` | The store or the file system failed; the caller reports the failure and keeps its own copy |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `put` | Reserve the file under the generation, write the bytes through FileStorage, commit the ownership row while the generation and the Session still exist | The reservation and the written bytes are abandoned; the sweep removes them; no owned row | The write completes under the owner's task, or the reservation expires with the generation; the sweep removes unowned bytes | Admission stops; admitted writes finish; a new generation sweeps the old one's pending rows | FileSystem by FileStorage; rows and sweep by SessionFiles |
| `import` | Open the source, check size, copy through FileStorage into a temporary file, commit ownership | The temporary file is removed; no owned row | The copy completes or is abandoned; the sweep removes what no row owns | As `put` | Source and temporary file by FileStorage; rows by SessionFiles |
| `read` | Resolve the file id under the generation's directory, read the bytes within the limit | The read stops; nothing changes | The read stops | Pending reads end `closed` | FileStorage |
| `register` | Check the Session and the output directory, insert the ownership row | None | The row stays; a later Session delete removes it | Rows stay for the next generation | SessionFiles |
| `owns` | Resolve the path, check the output row, else map the file directory and the file id row | The resolution stops | Nothing to release | Pending calls end `closed` | SessionFiles |
| `ownedPaths` | Read both ownership rows, map file ids to the directory | None | Nothing to release | Rows stay | SessionFiles |

A Session delete releases this owner's rows before the sweep runs; the sweep deletes bytes no row owns. Files another Session owns are never removed by one Session's delete.

## Exceptions with reasons

- The file directory is below the database directory: a Session's files are removed with the store that owns their rows, and the delete's last-owner guard is scoped to one database (ADR 013).
- The generation is the store's generation: a stale writer cannot commit into a Session that a newer store generation created.
- `read` answers the whole file within the owner's 16 MiB limit instead of paging: every caller needs the whole file, and a page protocol would push the read id's lifetime to callers.
- The ownership rows live in the engine's database but belong to this owner: the engine executes the transaction, SessionFiles decides when it commits.

## Verification

Flow tests on the owner:

- `put` reserves, writes and commits; a cancelled put leaves no owned row and the sweep removes the bytes; a second put with the same name gets a new file id.
- `import` refuses a directory, a non-regular file and a source over the limit without landing anything.
- `read` answers `not_found` for another Session's file id and for an unknown id; a file over the limit is never stored.
- `register` refuses a path outside the output directory; `owns` answers true for a registered output and for a Session file path, and false for another Session's file.
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
| §7.2 ids and values | File ids, names and bytes cross as data; a `fileId` is the resource id. |
| §7.2 one public face | The service is marked on the file owner's type under the existing `sessionFiles` name; the `SessionFiles` wrapper type and the `SessionFilesService` trait do not exist beside it. |
| §7.2 caller mechanisms | The reservation, the generation and the sweep stay inside the owner; callers handle the business errors only. |
| ADR 002 D1 | Process scope: the namespace and the rows outlive Sessions. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The owner depends on `fsHost`; callers depend on `sessionFiles`. |
| ADR 002 D4 | The 16 MiB limit is the owner's; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **FileStorage (#2016): the operations this owner calls.** `root_path`, `create_directory`, `write` with the read-only permission, `import` with `max_bytes` and the read-only permission, `list`, `remove`, `read`. [FileStorage's contract](https://github.com/arcships/dim/pull/2080) defines all of them; this contract raises no new need.
2. **Service type placement (#2015): a process owner in the app server.** The owner type is in `dim-app-server`, where the engine and the rows are; callers in plugin crates cannot depend on the app server. The request and reply types are plain data in `dim-agent-capabilities::host`, and callers outside the crate call the operations by the `sessionFiles` name. If the maintainers decide every process face must be annotated on a type plugin crates can name, the type moves to a shared crate; a comment on #2015 states the requirement.

## Binding appendix

The service is marked on the file owner's type; the wire name stays `sessionFiles`. The `SessionFiles` wrapper and `SessionFilesService` no longer exist. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFile {
    pub file_id: String,
    pub size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBytes {
    pub bytes: String, // base64 text
}

#[derive(Debug, thiserror::Error)]
pub enum SessionFileError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0} is over the Session file limit")]
    TooLarge(String),
    #[error("no such file: {0}")]
    NotFound(String),
    #[error("the resource node is unavailable: {0}")]
    NodeUnavailable(String),
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the file service is unloading")]
    Closed,
    #[error("{0}")]
    Storage(String),
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

    pub async fn read(
        &self,
        caller: &Ctx,
        request: ReadFile,
        cancelled: CancelledFn,
    ) -> Result<FileBytes, SessionFileError> {
        // ...
    }

    pub async fn register(
        &self,
        caller: &Ctx,
        request: Register,
        cancelled: CancelledFn,
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

    pub async fn owned_paths(
        &self,
        caller: &Ctx,
        request: OfSession,
        cancelled: CancelledFn,
    ) -> Result<Paths, SessionFileError> {
        // ...
    }
}
```

`SessionFiles` is the current `files::SessionFiles` owner type; the wrapper named `SessionFiles` in `dim-agent-capabilities` is deleted, not renamed. Every operation takes one request object, one reply object and the caller's `Ctx`; only the operations that wait take `CancelledFn`.
