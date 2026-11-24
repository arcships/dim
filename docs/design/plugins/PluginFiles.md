# PluginFiles

Scope: process service with row-bound scopes. Issue: #2016.

PluginFiles owns trusted-plugin file storage under one authorized product root: the version-1 text envelope, the per-consumer relative names, the 8 MiB whole-file bound and the exclusivity of one record transaction. It uses [FileStorage](../../architecture/rust-agent/plugins/FileStorage.md) for every read, write, directory and removal; it defines no file format or exclusion of its own beyond the envelope and the root. Its public face is the `dimcode/plugin` SDK (`npm/plugin.d.ts`). This contract replaces the lease surface in the App Server README, the TypeScript authoring guide and the public binding in process-typescript.

## Use and scope: responsibility split

PluginFiles owns:

- **The root and names.** One authorized product root per provider, and each consumer's relative path below it. A name is a relative path; `..`, an absolute path and the module's reserved names are refused.
- **The text envelope.** `{version: 1, text}` for files written by `withLock`; a file that is not version 1 fails `invalid_data` before a change runs and stays unchanged.
- **The whole-file bound.** `readBytes` and `replaceBytes` handle at most 8 MiB; the bound caps one allocation.
- **The transaction.** One record change is cross-process exclusive through FileStorage's `update`; the change receives the current text and returns the replacement and its own result.
- **The scope lifetime.** A scope is bound to the row that opened it; `quiesce` stops new work, `close` and row unload settle admitted work.

Callers keep:

- Their record formats and meanings: Scheduler jobs and runs, EventHub publications, Workflow runs and version token, Modality adapter files.
- Which relative name a record uses and what a result means.
- Their own side effects; an external effect is outside the transaction.
- Their own errors above PluginFiles' kinds.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read a small text record whole | Yes: `withLock` with a null replacement returns the current text | Business composes |
| Change a text record while other processes may write it | No: the read and the write must share one exclusion, and only the owner can hold FileStorage's `update` across the change | `withLock` |
| Read a large file in pages | No: the page bound and the size are the owner's, and the caller holds no reader | `read` |
| Read or replace a raw file whole | No: the 8 MiB bound is the owner's and one call must not hold it twice | `readBytes`, `replaceBytes` |
| Replace a file from a stream | No: the temporary file and the atomic rename are FileStorage's | `writeStream` |
| Create a work directory and learn its path | No: per-component validation is FileStorage's | `directory` |
| List one level and remove by a listed name | No: the name rules and removal are FileStorage's | `list`, `remove` |
| Stop and settle a scope | No: admitted work and the row lifetime are the owner's | `quiesce`, `close` |
| Back a file up before rewriting it | Yes: read the text, keep the copy in the caller's own record, then `withLock` | Business composes |
| Do several record edits under one lock | Yes, if they are one record change: compute them inside one `withLock`; otherwise they are several transactions and each is atomic | Business composes |

## Caller example

Workflow's run store keeps the run document and a token of the last committed write. Two records need two transactions; the token is derived state, so a reader whose token does not match rereads the document.

```ts
await scope.withLock("workflow/runs.json", () => ({ text: encode(runs), value: null }))
await scope.withLock("workflow/runs.version", () => ({ text: token, value: null }))
```

Scheduler's job record and EventHub's publish record change one text record per call:

```ts
const result = await scope.withLock("scheduler/jobs.json", (current) => {
  const jobs = decode(current) // null when absent
  const next = apply(jobs, request) // business rules
  return { text: encode(next), value: next }
})
if (!result.ok) return report(result.error)
```

A large file is read in pages by path, with no reader to release:

```ts
const page = await scope.read("modality/results/42.json", offset, PAGE)
if (page.ok && page.value) {
  if (page.value.eof) done()
  offset += page.value.data.length
}
```

## Own contract

### Dependencies

[FileStorage](../../architecture/rust-agent/plugins/FileStorage.md): the provider opens one authorized root (`add_root`/`root_path`) and calls `read`, `write`, `import`, `update`, `create_directory`, `list` and `remove`. No other service is injected.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `open` | Open a scope bound to the calling row | — | scope | While the root is opened | `invalid_path`, `io`, `closed`, `internal` |
| `withLock` | Change one text record under FileStorage's `update` | `{name}` and the `work` callback | the callback's own result | Before the replacement; the file is unchanged | `invalid_data`, `invalid_path`, `io`, `unsynced`, `cancelled`, `closed`, `internal` |
| `read` | One page of a file by path | `{name, offset, length}` | `{data, eof, size}` or none | Before the reply; nothing changes | `invalid_data`, `invalid_path`, `io`, `cancelled`, `closed` |
| `readBytes` | The whole file, envelope included, at most 8 MiB | `{name}` | bytes or none | Before the reply | `invalid_data`, `invalid_path`, `io`, `cancelled`, `closed` |
| `replaceBytes` | Replace the file with raw bytes, at most 8 MiB | `{name, bytes}` | unit | Before the replacement | `invalid_data`, `invalid_path`, `io`, `unsynced`, `cancelled`, `closed` |
| `writeStream` | Replace the file from a stream of chunks | `{name}` and the `next` callback | unit | Before the replacement; the old content stays | `invalid_path`, `io`, `unsynced`, `cancelled`, `closed` |
| `directory` | Make the work directory tree and return its absolute path | `{name}` | path | Between levels; created ancestors stay | `invalid_path`, `io`, `unsynced`, `cancelled`, `closed` |
| `list` | One level below `prefix` | `{prefix}` | entries | Before the reply | `invalid_path`, `io`, `cancelled`, `closed` |
| `remove` | Remove a file or a directory tree; an absent entry counts as removed | `{name}` | unit | Before the unlink; a tree can be partly removed, a retry is safe | `invalid_path`, `io`, `unsynced`, `cancelled`, `closed` |
| `quiesce` | Stop admitting work on the scope | — | unit | None; admitted work keeps its time | `closed` |
| `close` | Stop the scope and settle admitted work | — | unit | None; the owner completes | `closed` |

`withLock`'s `work` is a direct callback, not a request field. It receives the current text, or none when the file is absent, and returns the replacement text (none keeps the file unchanged, mapped to FileStorage's `update` with a `None` replacement) together with its own result. It only computes: an external wait, another owner or another PluginFiles call inside `work` holds the record for that whole wait. The owner runs `work` under FileStorage's `update`, so two processes cannot interleave; a repeated result is idempotent because the replacement is compared with the current text.

`read` returns a page of at most 64 KiB and the file's size; an offset equal to the size is an empty page with `eof`, an offset above it is `invalid_data`, and a missing file is none. `readBytes` and `replaceBytes` are one operation each and share the 8 MiB bound.

### Errors

| Kind | Retry | What the caller does next |
| --- | --- | --- |
| `invalid_path` | After fixing the name | A relative path escaping the root, an absolute path, a reserved name, or a name that is not one component; nothing changed |
| `invalid_data` | After fixing the data | A text file is not the version-1 envelope, a whole file is over 8 MiB, or a page offset is above the size; nothing changed |
| `io` | Yes, after fixing the environment | A filesystem refusal or an unavailable root; the message says which; a read shows whether the earlier call changed anything |
| `cancelled` | Yes, the same call | The caller's own cancellation ended the call before the replacement; nothing changed |
| `unsynced` | Not as an unchanged write | The bytes are in place but durability is unconfirmed; a read shows them and a repeated call confirms |
| `closed` | Yes, against a new scope | The provider or this scope is unloading; the operation did not run or its result is unknown past the replacement |
| `internal` | No | The host worker failed; the message is diagnostic, and the caller reports it without retrying the side effect |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `withLock` | Resolve the name, call FileStorage's `update`; `work` runs under that hold, the owner writes the replacement or leaves the file when it is none, and FileStorage syncs the parent | Before the replacement: the file is unchanged; the owner's `update` returns `cancelled` | Admitted work settles before the scope closes; an unadmitted call ends `cancelled` | New work is refused; admitted work settles; a later call is `closed` | FileStorage's `update` and temporary file; the scope by the calling row |
| `read` | Resolve the name, call FileStorage's `read`, return the page | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | FileStorage |
| `readBytes`, `replaceBytes` | One FileStorage `read` or `update`; the 8 MiB bound is checked before the call | Before the replacement | The same as `withLock` for `replaceBytes` | Pending calls end `closed` | FileStorage |
| `writeStream` | Resolve the name, pull the next chunk until none, FileStorage replaces atomically | The pull stops; the old content stays | The source is revoked; the old content stays | Pending calls end `closed` | FileStorage |
| `directory` | Resolve the name, FileStorage creates the tree, return the path | Between levels; created ancestors stay | The call ends `cancelled` | Pending calls end `closed` | FileStorage |
| `list`, `remove` | FileStorage `list`/`remove` on the resolved path | Before the reply or the unlink | The call ends `cancelled` | Pending calls end `closed` | FileStorage |
| `open`, `quiesce`, `close` | Bind or stop the scope; `close` joins admitted work | None | The row's unload closes the scope | A new scope needs a new provider generation | The scope by the calling row; the provider by its owner |

## Callers and migration

- Workflow's run store has one commit point. A write first writes the new content to `workflow/runs/<token>.json` (a new file), then makes one `update` of `workflow/runs.version`: the callback compares the current token with the one read before the write; on a mismatch it returns `None`, the writer deletes its new file, rereads and retries; on a match it writes the new token. That pointer update is the only commit; after it the writer deletes the old data file, and a failed delete leaves an orphan. A reader reads `runs.version` and reads the data file only when the token changed; if the data file is gone (a later write deleted it), it rereads the token and retries. Startup deletes `runs/*.json` the token does not point to, and when `runs.version` is absent but the old `runs.json` exists, it migrates once. A data-file write failure and a pointer-update failure are both uncommitted and return an error; there is no committed-but-unreadable state. Acceptance: a warm reader plus a write that fails between the data file and the pointer sees the old consistent snapshot and the writer gets an error; two concurrent writers, one retries and succeeds.
- Scheduler (jobs, runs, wakeups), EventHub (publish records) and Modality (adapter files) keep their record shapes and move to `withLock`, `readBytes`/`replaceBytes`, `writeStream` and `read`; each is checked in the delivery.
- `runtime/fixtures/files-*.ts`, `workflow-package.ts` and `crates/dim-app-server/tests/plugin_files_flow.rs` move to the new flows.

## Exceptions with reasons

- The scope is an object, not an id: it is the calling row's lifetime, `close` is registered with the row's effect, and the provider owns the root. The shared lifecycle is the process part's own; no caller holds it after its row unloads.
- `read` and `readBytes` return the stored bytes, envelope included, for a text file; decoding is the caller's. `withLock` is the only operation that handles the envelope.
- The 8 MiB bound is the caller's allocation bound, not FileStorage's document bound; a larger file is read in pages by path.

## Client-visible changes

The `dimcode/plugin` SDK is visible to third-party trusted plugins. This contract changes it:

- `FileLease` is deleted, with its `read`, `replace`, `backup`, `readBytes` and `replaceBytes` methods.
- `withLock(name, work, cancelled?)` takes a callback that receives the current text and returns `{text, value}`; it no longer yields a lease.
- `openRead` is replaced by `read(name, offset, length)`.
- `remove` no longer returns `locked`; it waits for the key.
- `FileError` drops `locked` and `exists` and adds `unsynced`.
- `npm/plugin.d.ts` changes in the implementation delivery together with the provider and every caller; this contract states the face they implement. The migration rows are in the delivery tables on [#2016](https://github.com/arcships/dim/issues/2016).

## Supersedes

- `crates/dim-app-server/README.md`: the "Trusted plugin storage" section.
- `docs/design/process-typescript.md`: §5.2 "Public PluginFiles binding".
- `docs/typescript-plugin-authoring.md`: the `withLock` transaction paragraph and the scope operations it lists.
- [FileStorage](../../architecture/rust-agent/plugins/FileStorage.md) § "Callers that hold work across an external wait": the PluginFiles paragraph.

## Verification

Flow tests on the owner:

- `withLock` runs the change under one FileStorage `update`; two processes changing one record serialize; a `null` replacement leaves the file unchanged; a change that fails leaves the old text.
- A non-version-1 envelope and a whole file over 8 MiB answer `invalid_data` without changing the file.
- `read` pages at 64 KiB with `size` and `eof`; an offset at the size is empty, above it is `invalid_data`; a missing file is none.
- `readBytes`/`replaceBytes` at the 8 MiB bound and just over it.
- `writeStream` publishes atomically and leaves the old content on a source failure or cancellation.
- `remove` of a held file waits and ends `cancelled`; an absent entry counts as removed; a tree retry after a partial removal finishes.
- `close` and row unload settle admitted work; a later call is `closed`.
- Migration checks: Workflow's two transactions (`workflow/runs.json`, then `workflow/runs.version`; the token is derived and a mismatched token rereads), Scheduler (jobs, runs, wakeups), EventHub (publish records) and Modality (adapter files) keep their record shapes; `runtime/fixtures/files-*.ts`, `workflow-package.ts` and `crates/dim-app-server/tests/plugin_files_flow.rs` move to these flows.

## Rule check

The rules are [design rules §7](../../architecture/rust-agent/principles.md#7-所有者与操作) and §1–§6 of `principles.md`.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process provider and one row-bound scope per caller; no Timeline fact and no Capability. |
| §2 scopes | The provider is process scope; the scope object is bound to the calling row and closes with it. |
| §3 orthogonality | Only FileStorage's operations are used; no other owner's state is read. |
| §4 rule 1 | No command event: PluginFiles writes no Timeline fact. |
| §4 rule 2 | No handoff: files and keys stay with FileStorage. |
| §4 rule 3 | The change runs under FileStorage's update, never inside the Timeline commit lock. |
| §4 rule 4 | Core and assembly carry no business meaning; no record format above the envelope appears here. |
| §4 rule 5 | No batch semantics. |
| §4 rule 6 | File content reaches the model only through tool results; PluginFiles only serves its callers. |
| §4 rule 7 | Callers use the scope operations; no caller reads a key or a descriptor. |
| §4 rule 8 | No mechanism requirement. |
| §4 rule 9 | No Timeline fact. |
| §7.1 resource and lifecycle | The provider owns the root and the scopes; a row's unload closes its scope after admitted work settles. |
| §7.1 operation granularity | One operation is one transaction or one read; no lease or reader id crosses a call. |
| §7.1 owner granularity | One storage owner for plugin files; FileStorage owns the bytes and the exclusion. |
| §7.2 operations | Every caller need maps to one operation above; nothing is proxied from another owner. |
| §7.2 handoff | Values and callbacks cross; the scope is the row's own lifetime with `close` registered by its effect. |
| §7.2 one face | One scope interface; no second API beside it. |
| §7.2 data face | Methods, arguments, replies and errors are data except the callbacks §7 allows; the scope object is the calling row's own. |
| §7.2 business boundary | No caller record format on the face beyond the version-1 envelope; each operation answers why it cannot be composed. |
| §7.2 one change | The SDK, the provider and every caller change in the same delivery. |

## Requirements raised to mechanism owners

None.

## Binding appendix

The SDK types are the public contract; the Rust binding is the provider's `PluginFiles` and `FileScope` with the same operations. `caller: &Ctx` is the dispatch's calling row.

```rust
use rutis::host::BoxFuture;
use rutis::Ctx;
use std::sync::Arc;

pub const WHOLE_FILE_LIMIT: u64 = 8 * 1024 * 1024;
pub const PAGE_LIMIT: u32 = 64 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
    pub eof: bool,
    pub size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Directory,
    Other,
}

/// The change's replacement (none keeps the file) and its own result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockChange<T> {
    pub text: Option<String>,
    pub value: T,
}

pub type WorkFn<T> = Arc<
    dyn Fn(Option<String>) -> BoxFuture<'static, Result<LockChange<T>, PluginFileError>> + Send + Sync,
>;

#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PluginFileError {
    #[error("{message}")]
    InvalidPath { message: String },
    #[error("{message}")]
    InvalidData { message: String },
    #[error("{message}")]
    Io { message: String },
    #[error("{message}")]
    Unsynced { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    Closed { message: String },
    #[error("{message}")]
    Internal { message: String },
}

pub struct PluginFiles;

impl PluginFiles {
    pub async fn open(&self, caller: &Ctx) -> Result<FileScope, PluginFileError> {
        // ...
    }
}

pub struct FileScope;

impl FileScope {
    pub async fn with_lock<T: Send + 'static>(
        &self,
        name: String,
        work: WorkFn<T>,
        cancelled: crate::host::CancelledFn,
    ) -> Result<T, PluginFileError> {
        // ...
    }

    pub async fn read(
        &self,
        name: String,
        offset: u64,
        length: u32,
        cancelled: crate::host::CancelledFn,
    ) -> Result<Option<Page>, PluginFileError> {
        // ...
    }

    pub async fn read_bytes(
        &self,
        name: String,
        cancelled: crate::host::CancelledFn,
    ) -> Result<Option<Vec<u8>>, PluginFileError> {
        // ...
    }

    pub async fn replace_bytes(
        &self,
        name: String,
        bytes: Vec<u8>,
        cancelled: crate::host::CancelledFn,
    ) -> Result<(), PluginFileError> {
        // ...
    }

    pub async fn write_stream(
        &self,
        name: String,
        next: Arc<dyn Fn() -> BoxFuture<'static, Result<Option<Vec<u8>>, PluginFileError>> + Send + Sync>,
        cancelled: crate::host::CancelledFn,
    ) -> Result<(), PluginFileError> {
        // ...
    }

    pub async fn list(
        &self,
        prefix: String,
        cancelled: crate::host::CancelledFn,
    ) -> Result<Vec<FileEntry>, PluginFileError> {
        // ...
    }

    pub async fn remove(
        &self,
        name: String,
        cancelled: crate::host::CancelledFn,
    ) -> Result<(), PluginFileError> {
        // ...
    }

    pub async fn directory(
        &self,
        name: String,
        cancelled: crate::host::CancelledFn,
    ) -> Result<String, PluginFileError> {
        // ...
    }

    pub fn quiesce(&self) {}

    pub async fn close(&self) {}
}
```

`base64_bytes` is the same small local serde module FileStorage uses for `Bytes`. Every callback is a direct parameter, not a request field.
