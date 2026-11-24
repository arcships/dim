# FileStorage

Owner: [file_storage.rs](../../../../crates/dim-agent-capabilities/src/file_storage.rs)
and [windows.rs](../../../../crates/dim-agent-capabilities/src/file_storage/windows.rs).

Target contract for [#2016](https://github.com/arcships/dim/issues/2016) and the current implementation.

Scope: process. Issue: #2016.

FileStorage owns byte storage under explicitly authorized roots. This document is the owner's target contract; the current implementation is described in [Current implementation](#current-implementation) below. PluginFiles is a separate owner under the same issue and is not covered here; its calls appear in the delivery tables on [#2016](https://github.com/arcships/dim/issues/2016).

## Use and scope: responsibility split

FileStorage owns:

- **Roots.** The process's authorized directories and their open handles. The host registers the roots it assembles with and the workspace of each Session it creates. `add_root` lets a trusted process part make a runtime directory addressable (Settings' project authorization, the UI extension directories, the migration source); `root_path` answers a root's directory for a caller that was given its id. Both are values, and neither grants authority: FileStorage is not a sandbox, a root only says which directories are addressable, and the trusted-plugin boundary and Permission decide what may be read or written.
- **Generations.** One service instance admits calls, owns the descriptors it opened and releases them on close.
- **Exclusion.** Each file's lock key beside the file; a credential record's lock is its published marker instead. A replacement, removal or update holds that file's lock (the key, or the credential record's published marker); another process holds the same lock for the same file under every case and Unicode spelling the filesystem equates. FileStorage waits for the lock until the caller cancels or the owner closes.
- **Credential markers.** `auth.json`, `mcp-oauth.json` and `chatgpt-account.json` use the published marker `<file>.lock` beside them: an exclusive file carrying the holder's process id, creation time and nonce. `claim` returns an id for one, `release` removes it, and a marker older than 30 seconds without a live process at its creation is reclaimed. Creation and reclamation of a marker are serialized by the private key for that file in the reserved directory; the key is held only while creating or reclaiming and is released before waiting, and it never crosses the face. Every operation that writes, removes or imports over a credential record takes its marker for the call, except `update_claimed`, which writes under the claim it is given; any holder, including the row that holds a claim, waits. A claim holder takes no other operation over that credential record that waits for the marker (`write`, `import`, `remove`, `update`); reads are taken while it holds the claim. A claim holder writes only through `update_claimed`; for a waiting operation it releases the claim first. An operation that takes the marker for its call removes it on cancellation, unload or owner close when it does not commit.
- **Atomic replacement.** A temporary file in the owner's namespace, flushed and renamed over the target, with the durability answer.
- **Reads and facts.** A whole document up to 8 MiB, one 64 KiB page at an offset, and a file's kind and size; reads are stateless, so no read id crosses.
- **Path validation.** Absolute paths inside a root, one validated component per open, no link followed, the owner's own resource names excluded.

Callers keep:

- Document formats and versions: settings documents, `auth.json`, `mcp-oauth.json`, `chatgpt-account.json`, the plugin envelope, JSONL records.
- Which root and file a business uses, and the file names.
- What a business conflict means and when to retry it.
- Permission and approval: FileStorage never decides whether a call may run.
- Workspace, system and worktree facts: FileAccess owns them; FileStorage answers only whether an absolute path is inside a root.
- Files a plugin creates and manages itself under a root, such as SQLite's database, the model-log recorder's JSONL and the UI extension cache.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one document: settings, accounts, plugin records, a project's hooks file | Yes: `read` without an offset reads the whole document and answers none when absent | Business composes |
| Read a large file in pages | No: the owner bounds one reply to a page, reports the end and the file's size, so the caller never holds an id or a page size of its own | `read` with `offset` and `length` |
| Learn a file's kind and size before reading or writing it (a permission fact, a publish) | No: existence, kind and length are one owner read, and a read cannot answer them without a file | `stat` |
| Replace a document's bytes atomically | Yes: `write` whose source yields the whole content once | Business composes |
| Write content that arrives in pieces, including a stream an adapter produces | No: holding it whole is unbounded; the temporary file and the key must stay inside the owner | `write` with a source |
| Read, decide and write back while other processes may replace the file | No: between a read and a write another writer replaces the file, so only the owner can hold the file across the caller's decision | `update` with a change |
| Keep the replaced bytes before a destructive rewrite | No: the copy must be created exclusively while the owner holds the file, and an existing entry at that path must block the rewrite | `update` or `update_claimed` with `keep_replaced_as` |
| Hold a credential record's marker across a token or refresh request | No: the marker is the published cross-process exclusion, and only the owner can create, reclaim and verify it | `claim`, `release`, and `update_claimed` |
| Refresh a credential | Yes: read the record, judge freshness, call the provider and write back inside the claim | Business |
| Hold a credential record's marker across a read and a publish without writing | No: the marker is the cross-process exclusion, and the read must see the record the holder later acts on | `claim`, `read`, `release`; a normalization write inside the hold uses `update_claimed` |
| Bring a host file the caller named into a root | No: the source is outside the roots and must be bounded and land atomically | `import` |
| Keep a document's file owner-only from creation (settings, accounts) | No: the mode is a file attribute the owner applies at creation and replacement | `permissions` on `write`, `import`, `update` and `update_claimed` |
| Write a Session file that `write` and `edit` may not modify | No: the read-only attribute is a file attribute the owner applies | `permissions` |
| Bound a host file import by the business limit (a Session attachment) | No: the caller's limit must stop the copy before it lands | `import` with `max_bytes` |
| Make a work directory tree and learn its absolute path | No: per-component link and alias validation is the owner's knowledge | `create_directory` |
| List one level and remove by a listed native name | No: the name's encoding and the single-component rule are the owner's knowledge | `list`, `remove` |
| Make a runtime-discovered directory addressable | No: the root set is the owner's state, and a root is a value other parts address | `add_root` |
| Hand a root's directory to another module: a process working directory, a path quoted in history, a log directory | No: the root set is the owner's state | `root_path` |
| Learn whether an optional file exists (a project hooks file, a Settings source) | Yes: `read` answers none; `stat` answers `missing`; `list` answers the parent's entries | Business composes |
| Decode or encode a document, choose its name and its root | Yes: formats and business paths stay with callers | Business |
| Append records to a log it owns (the model-log recorder) | Yes: the recorder owns the file; `root_path` gives it the directory | Business; the file is the recorder's, as SQLite's database is |
| Write a UI extension cache and then publish it | Yes: the cache belongs to UiExtensions; FileStorage reads it only on request | Business |
| Decide whether a read or write is allowed | Yes: Permission and Approval decide | Business |

An operation or parameter exists only where its row says No.

## Caller example

Reading one document, one page, and a file's facts:

```rust
// Whole document: none when absent, too_large above 8 MiB.
let Some(document) = storage.read(path.clone(), None, None, cancelled.clone()).await? else {
    return Ok(None);
};
let document = document.data;
// One 64 KiB page: size is the file length, eof says the page reached the end,
// and an offset past size is invalid rather than an empty page.
let page = storage.read(path.clone(), Some(offset), Some(PAGE), cancelled.clone()).await?;
// Existence, kind and size without reading the bytes.
let facts = storage.stat(path, cancelled).await?;
```

Settings' compare-and-set: the change runs under the owner's file key, sees the current content and returns the next content or a business refusal.

```rust
storage.update(
    path,
    Arc::new(move |current: Option<Bytes>| {
        Box::pin(async move {
            let current = decode(current.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
            let next = check_and_update(current, document.clone())?; // business rules, including the document's own revision
            Ok(Some(Bytes(encode(next)?)))
        })
    }),
    None,
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

DimOAuth's reset: keep the replaced bytes and rewrite the file in one `update`. The owner creates the copy exclusively under the file's lock (the key, or the credential record's marker) before the change runs; the change replaces the Dim fields and returns the next document.

```rust
storage.update(
    auth_path,
    Arc::new(move |stored: Option<Bytes>| {
        Box::pin(async move {
            let mut document = parse(stored.as_ref().map(|bytes| bytes.0.as_slice()))?; // business format
            document.remove(TOKENS);
            document.insert(STATE.into(), signed_out());
            Ok(Some(Bytes(encode(document)?)))
        })
    }),
    Some(backup_path), // absolute or bare auth.json.dim-reset-backup; an existing entry answers rejected/backup_exists
    Some(Permissions::Private),
    cancelled,
)
.await?;
```

UiResources' publish and pages, with no id to release:

```rust
// The size is checked at publish; a page request beyond it is the caller's invalid_params.
let facts = storage.stat(path.clone(), cancelled.clone()).await?;
let size = facts.size.unwrap_or(0);
let page = storage.read(path, Some(offset), Some(PAGE), cancelled).await?;
```

A module that needs a root's directory, such as the model-log recorder:

```rust
let home = storage.root_path(home_root)?;
let directory = format!("{home}/model-log"); // the recorder owns this file
```

## Callers that hold work across an external wait

The public face has no lease. Managed OAuth composes its exclusion from `claim`, `release` and `update_claimed`; Mcp does the same for its credential record; Migration and PluginFiles compose theirs from `update`, and the owner keeps the guarantee that one `update` is exclusive across processes.

**Managed OAuth, with a rotating refresh token.** `auth.json.lock` is the one exclusive lock for `auth.json`; there is no separate file key exposed to callers. `claim` and a directory removal that meets the marker take the private `auth.json` key while they create or reclaim it, release it before waiting, and re-acquire it after the wait; `release` removes the marker by matching its owner record without the key. Two reclaimers therefore cannot both create a marker, and a waiter never blocks the holder's release. The caller claims it with `claim(path, cancelled)`, which creates the marker with its process id, creation time and nonce and returns a claim id. The caller reads the record and judges freshness. The winner performs the HTTP refresh while it holds the marker, then writes the rotated record with one `update_claimed(caller, path, change, claim_id, keep_replaced_as?, permissions?, cancelled)`: the owner checks that the marker still carries this claim's nonce (a reclaimed claim answers `rejected` with reason `claim-lost`) and runs the change under that same hold without taking the lock again; the change compares the record's `credentialRevision` with the one it read, refusing with `rejected` reason `conflict` when another write advanced it. The caller then releases the claim. If the refresh fails, the caller releases the claim, re-reads the record and adopts it when `credentialRevision` advanced; otherwise it reports the failure. A waiter's `claim` waits for the marker; when it is released, the waiter re-reads the record, adopts it when `credentialRevision` advanced, and otherwise claims and refreshes itself. A `write` or `update` without a claim takes the same lock for its call, so another writer waits while the claim is held; a claim holder writes only through `update_claimed`; it releases the claim first for an operation that waits for the marker (`write`, `import`, `remove`, `update`).

Managed OAuth owns the refresh it issued. Before unload it finishes admitted work, and a cancellation waits for the disk write before returning; an admitted `update_claimed` settles before the claim is released, while an unadmitted call ends `cancelled`. On row unload or owner close, FileStorage waits for this row's admitted `update_claimed` to settle (the write or an explicit failure) before releasing the claim, so a provider rotation cannot outlive the local claim. The flow tests assert `refreshes == 1` with two callers and a rotating token, a write attempted between claim and commit waits for the marker, an advanced `credentialRevision` is refused with `conflict`, a reclaimed claim answers `claim-lost`, a failed refresh with a waiting loser lets the waiter claim and refresh, an external marker-following writer blocked during the refresh, and a row unload between the refresh and its write that settles before the claim is released.

If the refresh succeeds but its write-back does not, `io` or `closed` means the record was not written; `rejected` with reason `conflict` or `claim-lost` means the same. Managed OAuth returns the error, leaves the record as it was and releases the claim. `unsynced` means the content is written but durability is unconfirmed; the caller treats it as success. If the provider already rotated and the record still holds the older refresh token, the next refresh is rejected by the provider and the record becomes `rejected`; the user signs in again ([managed_oauth.rs:1073-1077](../../../../crates/dim-app-server/src/managed_oauth.rs)). Row unload ends the row's admitted refresh, HTTP and its last `update_claimed`, before the release; FileStorage's row-unload release is the fallback after that work settles ([managed_oauth.rs:245-273](../../../../crates/dim-app-server/src/managed_oauth.rs)).

**DimOAuth refresh.** The DimOAuth store claims `auth.json.lock` before a refresh, re-reads the record, calls the provider and writes the rotated grant with one `update_claimed`; the change compares `credentialRevision` and refuses an advanced one with `rejected` reason `conflict`. A rejected grant writes the signed-out state under the same claim. The claim is released after the write, successfully or not.

**MCP credential record.** `mcp-oauth.json.lock` is the one exclusive lock for `mcp-oauth.json`, under the same marker protocol as `auth.json`. The file has one writer, but two daemons can share a home (for example `--product-home` with different `--database` paths), and the marker stops both from redeeming one rotating refresh token. Mcp claims the marker with `claim(path, cancelled)` before a code exchange, refresh or revocation, re-reads the record under it, and holds it through the token request. The write-back is one `update_claimed` whose change replaces only the caller's key in the record; the owner checks the marker's nonce and runs the change under that hold. A failed request releases the claim without writing. Discovery and registration run outside the marker. Every token request, for any server, holds the one marker, so requests for different servers wait on each other; each is bounded by its 30-second timeout, and they are rare. A token request that has been sent is received and persisted on McpAuth's own task, whoever waits for it; Mcp's unload waits for it, and FileStorage's row-unload release waits for the row's admitted `update_claimed` to settle, so a rotated refresh token or a redeemed code is not lost. A code exchange whose attempt was superseded, or whose record cannot be written, is still received, but its tokens are revoked and discarded. The ending follows the error list above: `io` or `closed` means the record was not written, and `rejected` with reason `claim-lost` means the same; `unsynced` means the content is written and durability is unconfirmed; a claim asserted by another daemon makes the caller wait, not fail.

**ChatGPT account record.** `chatgpt-account.json.lock` is the one exclusive lock for `chatgpt-account.json`; ManagedAccount is the file's only writer, and the marker serializes a refresh, a code exchange and a sign-out with any other writer of the record. Each of the three claims the marker before its network call, reads the record, calls the provider, and writes with one `update_claimed` whose change replaces the active profile's credentials or clears them. The claim is released after the write, successfully or not. A safe status read does not start a refresh and reads the file without the marker; startup never starts one. If the write fails, `io` or `closed` means the record was not written, `rejected` with reason `claim-lost` means the same, and `unsynced` means it was written and durability is unconfirmed. A claim from a previous account generation cannot refresh or invalidate the current one.

**Migration snapshot run.** Migration defines its run claim in its own document; the FileStorage operation it uses is `update`. The claim is a file whose change writes the content only when the file is absent and otherwise refuses with a `rejected` error whose reason is `held`; a later run checks the recorded process with a credential marker's rule (the process id is alive and its start time is not later than the claim's creation) and replaces a stale claim inside the same `update`. No key is held while a run copies files.

**PluginFiles.** PluginFiles is a separate owner under the same issue; it owns the record envelope and its callers' migration, and it composes its record transaction from one `update`.

## Own contract

### Dependencies

None. FileStorage injects no service and reads no other owner's state. The host registers roots through the provider when it assembles the process and when it creates a Session; trusted process parts add runtime roots with `add_root` and read a root's directory with `root_path`. The provider is chosen by deployment ([resource faces §1](../../../design/resource-faces.md#1-scope)): whether a call reaches a local or a mounted instance is not a property of this name.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `add_root` | Make an existing absolute directory addressable | `{path}` | `root`; an existing root answers its id | Before the directory handle is opened | `invalid`, `io`, `closed`, `cancelled` |
| `root_path` | A root's absolute directory for another module | `{root}` | `path` | None; the owner completes the read | `invalid`, `closed` |
| `stat` | A path's kind and size | `{path}` | `Stat`; `missing` is a kind, not an error | Before the reply | `invalid`, `io`, `closed`, `cancelled` |
| `read` | Read a whole document, or one page of a file | `{path, offset?, length?}` | `ReadReply {data, eof, size}` or none | Before the reply; nothing changes | `invalid`, `too_large`, `io`, `closed`, `cancelled` |
| `claim` | Hold a credential record's published marker | `{path}` | `claimId`; the marker carries this caller's nonce | While waiting for the marker; no marker is created. If cancelled after creation but before the id is returned, the owner removes that marker | `invalid`, `io`, `closed`, `cancelled` |
| `release` | Remove the credential marker this caller holds | `{claimId}` | unit | None; the owner completes the removal | `invalid`, `rejected`, `io`, `closed` |
| `write` | Replace a file's bytes atomically from a source the owner pulls to its end | `{path, permissions?}` and the `source` callback | `size` | Before the replacement; the temporary file is removed | `invalid`, `missing_parent`, `rejected`, `io`, `unsynced`, `closed`, `cancelled` |
| `import` | Copy a regular host file into a root atomically | `{path, source, maxBytes?, permissions?}` | `size` | Before the replacement; the temporary file is removed | `invalid`, `too_large`, `missing_parent`, `io`, `unsynced`, `closed`, `cancelled` |
| `update` | Change a file's content while the owner holds the file across the caller's decision | `{path, keepReplacedAs?, permissions?}` and the `change` callback | `UpdateReply {applied}` | Before the replacement; the temporary file and any copy are removed | `invalid`, `missing_parent`, `too_large`, `rejected`, `io`, `unsynced`, `closed`, `cancelled` |
| `update_claimed` | The same change under the credential marker this row holds | `{path, claimId, keepReplacedAs?, permissions?}` and the `change` callback | `UpdateReply {applied}` | Before the replacement; the temporary file and any copy are removed | `invalid`, `missing_parent`, `too_large`, `rejected`, `io`, `unsynced`, `closed`, `cancelled` |
| `create_directory` | Make the path an existing directory tree | `{path}` | `path` | Between levels; created ancestors stay, a retry is safe | `invalid`, `io`, `unsynced`, `closed`, `cancelled` |
| `list` | One level of a directory; the owner's own resources, including equivalent spellings, never appear | `{path}` | `entries` or none when the directory is absent | Before the reply | `invalid`, `io`, `closed`, `cancelled` |
| `remove` | Make a file or a directory tree absent; an absent entry counts as removed | `{path}` or `{parent, name}` | unit | Before the entry is unlinked; a tree can be partly removed, a retry is safe | `invalid`, `io`, `unsynced`, `closed`, `cancelled` |

`read` without `offset` and `length` reads the whole document up to 8 MiB and answers `too_large` above it; with both, it reads one page of at most 64 KiB (`length` above the page is clamped), `size` is the file's length and `eof` is true when the page reaches the end; one of the two alone is `invalid`. An offset equal to `size` reads an empty page with `eof`; an offset above `size` is `invalid`, and a `length` of 0 is `invalid` (the client paging contract depends on the distinction). An absent file answers none, and so does a missing parent directory. A whole-document read reports `eof: true`. `Bytes` crosses as base64 text, so 8 MiB of bytes fits the 16 MiB bridge frame with the JSON envelope.

`write`'s `source` and `update`/`update_claimed`'s `change` are direct method parameters, not request fields. `source` is called repeatedly until it returns none: each call yields the next chunk of at most 8 MiB of raw bytes, or a `Rejected` error that ends the write and leaves the target unchanged; a larger chunk is `invalid`. The parent is checked before `change`: a missing parent answers `missing_parent` and the change does not run. `change` is called once while the file's lock (the key, or the credential record's marker) is held, with the current content (at most 8 MiB) or none when the file is absent, and returns the next content (`Some`), no write (`None`) or a `Rejected` error. `None` leaves an existing file unchanged and does not create an absent file, still runs under the file's key and answers `applied: false`. A returned content above 8 MiB is `too_large`; `applied` is false when the content equals the current one, so a repeated write is idempotent. `update` takes no caller; `update_claimed` takes `caller: &Ctx` because the claim is bound to the row. `change` must only compute: waiting on another owner, on network I/O or on FileStorage itself inside `change` holds the file for that whole wait. §7 allows a callback by reference, and the change is never called inside the Timeline's commit lock.

`keep_replaced_as` is either an absolute path in the target's parent or a bare name the owner joins to that parent. Under the file's lock (the key, or the credential record's marker) the owner reads the current content, copies it to the path exclusively before calling `change`: an existing entry at that path answers `rejected` with reason `backup_exists` and the change does not run; an absent target, an absolute path outside the target's parent, the target itself or a reserved name is `invalid`. The copy is `private` (0600). Any failure while creating, writing or syncing the copy removes the partial copy and syncs the parent before returning `io`. If `change` returns `None`, refuses, returns content equal to the current one (`applied: false`), returns content above 8 MiB (`too_large`), or the replacement fails before the rename, the owner removes the copy and syncs the parent, so a later call is not blocked by a stale copy. A committed replacement, including `unsynced`, leaves the copy. A crash between the copy and the replacement can leave a copy of bytes that are still the file; `backup_exists` reports it and the caller resolves it deliberately. `update_claimed` does the same under the held marker.

`claim`'s `path` is a credential record: `auth.json`, `mcp-oauth.json` or `chatgpt-account.json`; the owner derives that file's published marker beside it and answers `invalid` for any other path. A claim is bound to the caller's row for ownership and cleanup: the row's unload releases it and `release` accepts only the claim that row holds. A second `claim` from the same row, including one for the same record, waits like any caller and can cancel; only one refresh or token request per row runs at a time. `update_claimed` checks the marker's nonce before the change runs; a reclaimed claim is `rejected` with reason `claim-lost`. A write or update of a credential record without a claim takes its marker for the call; a claim holder passes `claim_id` only to `update_claimed`; it releases the claim first for an operation that waits for the marker (`write`, `import`, `remove`, `update`).

`permissions` is absent for "keep the existing mode; a new file gets `private`": `private` is owner-only (0600), `privateReadOnly` is owner-only and read-only (0400), the attribute Session files use. `max_bytes` is absent for the owner's 8 MiB default; a caller with a different business cap passes it. A listed name is passed back to `remove` unchanged. FileStorage waits for a file's key until the caller cancels or the owner closes; there is no fixed bound, so a call that must not wait ends with its cancellation.

### Errors

| Kind | Retry | What the caller does next |
| --- | --- | --- |
| `invalid` | After fixing the input | A relative path, a destination outside every root, a root's own path, an unknown root id, the owner's reserved names (the lock directory, its temporary directory and the credential markers, including filesystem-equivalent spellings), a name that is not one component, a directory where a file is required, a file where a directory is required, a link where a directory or a file is required, one of `offset` and `length` without the other, an offset above the file's size, a `length` of 0, a source chunk above 8 MiB, a claim path that is not a credential record, an unknown or other-row claim id, `keep_replaced_as` outside the target's parent, equal to the target or set when the target is absent; for `import`, a source that is relative, missing, not a regular file or a link |
| `missing_parent` | After creating the parent | Call `create_directory` or fix the path; the owner changed nothing |
| `too_large` | For `read`, yes in pages; for `import`, after fixing the source | `read`: use `offset` and `length`, or treat it as not a document; no partial bytes were returned. `import`: the source exceeds `max_bytes` or grew during the copy; nothing was published. `update` or `update_claimed`: the current or returned content is above the document limit; treat it as not a document |
| `rejected` | Depends on `reason` | The caller's own `change` or `source` refused with its `reason` and `message`, the claim was reclaimed (`claim-lost`), or `keep_replaced_as` found an existing entry (`backup_exists`); act on `reason`, for example re-read and retry the business step on `conflict`, re-claim after `claim-lost`, or move the retained copy before retrying `backup_exists`. A revoked callback is `cancelled`, not `rejected` |
| `io` | Yes, after fixing the environment | The filesystem refused the operation or the import source shrank while copying: a permission, a device, a network or an unsupported-filesystem error. The message and optional path say which; a `read` or `stat` shows whether the earlier call changed anything |
| `unsynced` | Not as an unchanged write | Durability is unconfirmed: after `write`, `import`, `update` or `update_claimed` the new bytes are in place; after `create_directory` the directory is; after `remove` the entry is absent. A read or stat shows the state, and a repeated call confirms |
| `cancelled` | Yes, the same call | `read`, `write`, `update`, `update_claimed` and `import` changed nothing before the replacement; `import` closes the source, `claim` creates no marker or removes one it created before returning the id, `create_directory` and `remove` follow their cancellation columns: created ancestors stay, a tree can be partly removed |
| `closed` | Yes, against the replacement generation | This generation is unloading. For `write`, `import`, `update` and `update_claimed`, `closed` means the call ended before the replacement and the marker it took is removed; after the replacement it succeeds or reports `unsynced`. For `remove` and `create_directory`, follow their cancellation columns. A `read` or `stat` answers which; an `add_root` either appended the root, so a later call answers its id, or did not; a `claim` either created no marker or the row's cleanup removes it; a `release` either removed the marker or the caller retries against the replacement generation; a claim id from a previous generation answers `closed`, and the caller re-claims. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `add_root` | Open the directory handle, append the root; an existing root answers its id | Before the handle is opened or appended; nothing is appended | The call ends `cancelled` | New calls are refused; the append-only root table stays for the next generation, so a root id stays valid | Directory handle and root table by FileStorage |
| `root_path` | Read the root's directory from the root table | None; the call completes | The call completes | A later call is `closed`; a root id stays valid across the reload | Value by FileStorage |
| `update_claimed` | Check the marker's nonce and run under that hold without taking the lock again. Read the current content within the document limit; with `keep_replaced_as`, create the copy exclusively and sync it, then call `change`; `None` writes nothing and removes the copy, `Some` writes the temporary file, flush, rename, sync parent. The claim stays with the calling row | Before the rename: temporary file and copy removed, target unchanged, `cancelled`. After the rename: success or `unsynced`, the write is not undone and the copy stays | An admitted call settles (its change stays callable) before the row's claim is released; a copy this call created is removed when it does not commit | New calls are refused; in-flight calls finish or end `closed`; a copy this call created is removed when it does not commit | Marker, temporary file and copy by FileStorage; the claim by the calling row |
| `claim` | Take the private key for the record's file, then create its published marker exclusively with this process's id, creation time and nonce; a stale marker is unlinked and recreated under the same key and read back to verify its nonce. A live marker, a marker this row already holds, or a lost recreate race releases the key, waits for the marker to clear or change, then re-acquires the key and re-reads; the winner never waits while holding the key | Before the marker is created; a waiter's wait ends `cancelled`; no key is held while waiting. After creation, cancellation removes the marker by its nonce before the id is returned | The row's admitted `update_claimed` calls settle first, then the claim is released and the marker removed | Pending claims end `closed`; after the row's admitted `update_claimed` calls settle, the marker is removed | Marker by FileStorage; the claim id's lifetime by the calling row |
| `release` | Remove the marker whose owner record matches the claim, without taking the private key; a marker another reclaimer now holds answers `rejected` with reason `claim-lost` | None; the owner completes the removal | The row's claim is released | A later call is `closed`; the marker is removed | Marker by FileStorage |
| `stat` | Open the parent and the entry without following links, read the kind and size, release | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | Descriptor by FileStorage |
| `read` | Open the parent and the file without following links, read the whole document or the page, report `size` and `eof`, release | Before the reply; nothing changes | The call ends `cancelled` | Pending calls end `closed` | Descriptor by FileStorage |
| `write` | Parent handle, the file's key (`auth.json`, `mcp-oauth.json` and `chatgpt-account.json` take their markers), temporary file, pull the source to its end, flush, rename, sync parent, release key; a credential record's marker is removed after the replacement | The pull stops, the temporary file is removed, the target is unchanged, `cancelled` | The source callback is revoked; the pull fails; the temporary file is removed, `cancelled` | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file | Temporary file and key by FileStorage; the source's future by the calling row |
| `import` | Open the source without following links, require a regular file at its initial length, copy into a temporary file, verify the source kept that length (growth is `too_large`, shrink is `io`, neither publishes), rename, sync parent; a credential record takes its marker, releases the private key once the marker is created, and after the rename removes the marker; a non-credential target takes its key; a claim holder releases the claim first | The temporary file is removed, the target is unchanged, the source closes | The call ends `cancelled` | Pending calls end `closed` | Source descriptor, temporary file and key by FileStorage |
| `update` | Take the file's key (`auth.json`, `mcp-oauth.json` and `chatgpt-account.json` take their markers) for the call. Read the current content within the document limit; with `keep_replaced_as`, create the copy exclusively and sync it, then call `change`; `None` writes nothing, removes the copy and releases the key, `Some` writes the temporary file, flush, rename, sync parent, release; a credential record's marker is removed after the replacement | Before the rename: temporary file and copy removed, target unchanged, key released, `cancelled`. After the rename: success or `unsynced`, the write is not undone and the copy stays | The callback is revoked and the call ends `cancelled`; a copy this call created is removed when it does not commit; the same release applies | New calls are refused; in-flight calls finish or end `closed`; the generation releases every key and temporary file and removes a copy this call created when it does not commit | Key, temporary file and copy by FileStorage; the change's future by the calling row |
| `create_directory` | Each missing level below the held parent, mode 0700, sync the parent | Between levels; created ancestors stay | The call ends `cancelled` | Pending calls end `closed` | Created directories by FileStorage |
| `list` | Open the directory, read one level, sort by name | Before the reply | The call ends `cancelled` | Pending calls end `closed` | Directory handle by FileStorage |
| `remove` | Parent handle, the entry's key; a credential record takes its marker (a claim holder releases the claim first), and a directory holding a credential marker is handled under that record's private key, released before waiting: a live marker waits, a stale one is reclaimed by `claim`'s rule, and the marker is never removed as a business entry; once the entry is absent, a credential record's marker and private key are removed, and a directory removal does the same for every credential marker under its private key; remove depth first without following links, sync the parent | Before the unlink: nothing removed; a tree can be partly removed, a retry is safe | The call ends `cancelled` | Pending waits end `closed`; the generation releases keys | Entry removal by FileStorage |

## Exceptions with reasons

- `import` reads a host path outside the roots: the caller names a file the user or model chose (an attachment); the owner checks that the source is a regular file, does not follow a link and enforces the byte limit, while the root constrains only the destination.
- The credential markers `auth.json.lock`, `mcp-oauth.json.lock` and `chatgpt-account.json.lock` are the published exclusions for `auth.json`, `mcp-oauth.json` and `chatgpt-account.json`: every writer of the file, in this process or another, holds the same marker. A marker is a file carrying the holder's process id, creation time and nonce; a marker older than 30 seconds without a live process at its creation is reclaimed, and a marker this process failed to release is reclaimed at once by its next claim. `auth.json.lock` coordinates the two Rust writers of `auth.json` — the DimOAuth store and managed OAuth; the TypeScript plugin no longer writes the file ([runtime contract](../../../../crates/dim-oauth/docs/runtime-contract.md) line 32), so it is not a cross-language protocol. `mcp-oauth.json.lock` coordinates daemons that share a home while Mcp is the file's only writer. `chatgpt-account.json.lock` coordinates the ChatGPT account's refresh with any other writer of the record.
- `Rejected` carries only `reason` and `message`: a callback error must cross languages, so it comes back as data; `From<CallError>` recovers the wire error's own code into `reason`, and the caller keeps its business error type inside its own callback.

## Client-visible changes

None at the client protocol. This contract changes the plugin-facing `fsHost` surface: `read(path, offset, length)` replaces the pinned reader, `add_root` and `root_path` are operations returning addressability values, `update`/`update_claimed` take `keep_replaced_as`, a credential record's `claim`/`update_claimed` replaces a lease, and the `fileStorage` service name is removed: its consumers call `fsHost` on the `FileStorage` type. PluginFiles' `FileScope` changes are that owner's contract in the same issue ([#2016](https://github.com/arcships/dim/issues/2016)). Client protocol error codes, notification names, `session.ui.resources.read` (its request, `total` and `bytes`), Session file offset paging, the `sessionFiles.import` reply's `path`, model-visible text, and disconnect and progress behavior are unchanged. UiResources pages by path through `read`'s offset and length; its contract ([#2021](https://github.com/arcships/dim/issues/2021)) records the size at publish and requires a publisher to keep a published path unchanged. A rewrite under a published reference is a UiResources contract violation. The pinned reader is deleted in the same delivery as this contract; UiResources records the published `size` and compares each `ReadReply.size`, answering `resource_gone` when it changes. A same-size rewrite is undetectable by path paging, so the publisher's rule not to rewrite a published path is the only version guarantee; a rewrite is a UiResources contract violation.

## Supersedes

- `docs/design/resource-faces.md`: the status header, the `fsHost` row of §1, §2 rules 3–4, §3.2, §3.4, the §1 diagram and introduction (including the introduction that maps `fsHost`), the §4 heading, the storage-family table and the tool-family note, §4.2.1, §4.3, §4.6, §4.7, §6 (the `fileStorage` row and its §6 caller rows) and §10.
- `docs/design/contracts/resource-owner-operations.md`: the `fileStorage` row, the resource-caller row, the "OS path and input reader" paragraph and the file-backed Settings sentences.
- `docs/design/service-faces.md`: the `fileStorage` row of §3.4 and the `fileStorage` sentence in the paragraph after the table.
- `docs/design/appserver/session-files.md` and its `docs/zh` mirror: the overview line, the FileStorage bullet of §1, the §2 diagram and §3.1, §5.1, §5.4, §6, §7, §10, §12, §13, §15, §14, the `create_directory` acceptance of §16 item 8, §17 D1 and Appendix A's `create_directory` signature and root-id paragraph.
- `docs/design/p6-p7-bindings.md`: the Workflow definitions row of §1, and the `createDirectory` row, root-registration paragraph, private-type sentence, definition-read paragraph and error distinctions of §3.1.
- `docs/architecture/rust-agent/plugins/README.md`: the FileStorage index row.
- `docs/architecture/README.md` and `docs/architecture/rust-agent/single-tree.md`: the lease sentences of the FileStorage generation.
- `docs/design/overview.md` and its `docs/zh` mirror: the B.1 FileStorage supplement row.
- `crates/dim-app-server/README.md`: the FileStorage root-id sentence.
- `docs/design/plugins/Modality.md` and its `docs/zh` mirror: the file-read clause of the dependency table.
- `crates/dim-oauth/docs/runtime-contract.md`: the account record and login paragraphs' lease, lock and reset-backup sentences.
- `docs/design/appserver/session-deletion-and-fork.md` and its `docs/zh` mirror: the FileStorage key and `replace` references, the Session-lease labels, the root-id sentence, §3.5, the error table, §4, §7's S6 paragraph, §12 and the §14 row.
- `docs/architecture/rust-agent/plugins/Hooks.md`: the `readBytes` sentence.
- `docs/design/plugins/Hooks.md` and its `docs/zh` mirror: the `.dimcode/hooks.json` binding row.
- `docs/architecture/rust-agent/plugins/HostPlatform.md`: the lease sentence.
- `docs/design/appserver/mcp-oauth.md` and its `docs/zh` mirror: the §1 basis row and the credential store's lock, write-path, sign-out and storage sentences.
- `docs/design/client-ui-extensions.md` and `docs/architecture/client-ui-extensions.md`: the UiExtensions and UiResources dependency rows, the reading-resource and read-handle sentences, the §9 implementation-order row, and R5.
- `docs/zh/design/appserver/session-files.md`: the ADR 013 link repair.

Handed to PluginFiles in the same issue ([#2016](https://github.com/arcships/dim/issues/2016), batch 2): the `pluginFiles` row and the `FileScope.openRead`/`writeStream` rows of `docs/design/contracts/resource-owner-operations.md`, `docs/design/process-typescript.md` §5.2, `docs/typescript-plugin-authoring.md`, and the trusted-plugin storage section of `crates/dim-app-server/README.md`.

## Verification

The caller examples compile against the binding in a temporary crate outside the repository; the organizer runs that check.

Flow tests on the owner:

- `read` answers none for an absent file, `too_large` above 8 MiB for a whole read, `size` and `eof` for a page, an empty page at `offset == size`, and `invalid` at `offset > size`.
- A bridge flow test near 8 MiB: the whole-read reply crosses the 16 MiB frame, and a source chunk above 8 MiB answers `invalid`.
- `read` stays stateless: no id is issued, and a page read after a replace sees the replaced content.
- `stat` answers `file`, `directory`, `other` and `missing`, with the size for files and none for the others.
- `write` pulls a multi-chunk source, commits atomically, removes the temporary file when the source fails or is revoked, and leaves the target when cancelled.
- `update` calls the change under the key; a change that returns `Rejected` leaves the file unchanged; an identical result reports `applied: false`; a cancel before the rename removes the temporary file; a failed parent sync reports `unsynced`. `update_claimed` does the same under a held marker and answers `rejected` with reason `claim-lost` once the marker was reclaimed.
- `keep_replaced_as` copies the current bytes exclusively before the change; an existing entry answers `rejected`/`backup_exists` and the change does not run; a bare name is joined to the target's parent; a copy creation, write or sync failure removes the partial copy, syncs the parent and answers `io`; a change that returns `None`, `Rejected`, content equal to the current one, or an oversized `Some` (`too_large`), a cancellation before the rename, or a failed replacement removes the copy, so a later reset is not blocked; two concurrent resets leave one copy and one replacement; a concurrent `auth.json` write waits for the marker or is included in the copied bytes, and a refused change leaves no stale copy.
- `create_directory` creates each missing level below the root with mode 0700 and syncs the parent; an existing directory succeeds; a missing intermediate level is created, not an error; reserved names, their case aliases and an existing file or link at any level are `invalid` and create nothing; cancellation between levels leaves the created ancestors.
- `write`, `update`, `remove` or `import` over a credential record leaves no marker; a later `claim` proceeds without waiting.
- `import` over `max_bytes` copies nothing; a missing, non-regular or linked source is `invalid`; a destination outside every root is `invalid`.
- `list` answers one level sorted by name, omits the owner's lock, temporary and marker resources including case aliases, and answers none for an absent directory.
- `remove` of a tree with a file another writer holds waits, and cancelling ends the wait with `cancelled`; a retry after a partial removal finishes; a link is not followed.
- `add_root` answers the same id for the same path; `root_path` answers that directory; an unknown root id is `invalid`; a root id registered before a provider reload answers its directory after the reload.
- An `update` with a `None` replacement on an absent file creates nothing, answers `applied: false`, and a concurrent creator takes the key after it; an existing file stays unchanged with `applied: false`.
- The DimOAuth refresh tests: a refresh claims the marker, persists a rotated grant with one `update_claimed`, and a concurrent refresh waits and adopts it; a rejected grant writes the signed-out state under the same claim.
- A claim holder reads the record, calls `update_claimed` and only then releases; a `write`/`import`/`remove`/`update` attempted while the claim is held waits until release; a second `claim` waits.
- The managed OAuth flow tests: two concurrent callers and a rotating refresh token perform one refresh (`refreshes == 1`) and both read `rotated-1`; a write attempted between claim and commit waits for the marker; a change that sees an advanced `credentialRevision` refuses with `rejected` reason `conflict`; a reclaimed claim answers `rejected` with reason `claim-lost`; a failed refresh with a waiting loser lets the waiter claim and refresh; a writer that follows the marker waits while another caller holds the claim.
- The MCP credential flow tests: two daemons sharing a home perform one refresh while the marker is held; a write between claim and commit waits for the marker and then sees the rotated record; a sign-out holds the marker across revocation and removes only its own key's entry with `update_claimed`, leaving the file and other entries; a row unload between the token response and its write settles before the claim is released; a rejected refresh removes only its own key's record.
- The ChatGPT account flow tests: two concurrent refreshes with a rotating token perform one while the marker is held; a code exchange and a sign-out each hold the marker across their network call and persist with one `update_claimed`; a sign-out between a refresh's response and its write waits for the marker; a safe status read starts no refresh; a row unload between the response and its write settles before the claim is released; a claim from a previous account generation cannot refresh or invalidate the current one.

Material uncertainty: a change that waits on another owner or on I/O holds the file's key for that wait. The contract forbids it; review checks each caller's change.

## Rule check

The rules are [design rules §7](../principles.md#7-所有者与操作) and §1–§6 of `docs/architecture/rust-agent/principles.md`.

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
| §7.1 resource and lifecycle | FileStorage owns the roots, keys, descriptors and generations; the host registers roots, the owner settles keys and temporary files on close and reload. |
| §7.1 operation granularity | One operation is one state transition or one consistent read; read ids, keys and temporary files never cross a call. |
| §7.1 owner granularity | One process owner for byte storage under roots; no second owner beside it. |
| §7.2 operations | Every cross-plugin call lands on an operation here; the sibling contracts' operations exist. |
| §7.2 handoff | Only values (paths, sizes, root ids, claim ids), read-only views (bytes, entries) and callbacks (source, change) cross; no object or handle. |
| §7.2 no proxying | FileStorage returns no other owner's resource or data. |
| §7.2 one face | One operation set on the owner type; no forwarding facade, command or host object. |
| §7.2 data face | No Rust-only method; caller-supplied arguments, replies and errors are data except the callbacks §7 allows, and `caller: &Ctx` is the dispatch's row identity, filled by the dispatch and not caller-supplied data. |
| §7.2 business boundary | No caller business concept on the face; each operation and parameter answers why it cannot be composed, in the table above. |
| §7.2 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Process scope: roots, descriptors, keys and generations outlive Sessions and are shared across them. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; no adapter or facade exists beside it. |
| ADR 002 D3 | Dependencies point from extensions to the core; FileStorage depends on nothing and is a process service they may depend on. |
| ADR 002 D4 | Roots are structural: the host registers them, and a trusted business part may register a directory it owns; a root is addressability, and Permission decides what an agent action may do there. |
| ADR 002 D5 | The contract is language-neutral: operations, data and callbacks; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

None.

## Binding appendix

The service is marked on the owner type; the wire name stays `fsHost`. The `FsHost` wrapper type no longer exists. `CancelledFn` is the binding's cancellation argument, not a request field. Error codes are the snake_case `code` values of the error table.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use rutis::Ctx;
use std::sync::Arc;

/// Raw bytes of a whole read, an update, or one source chunk. Base64 on the wire,
/// so 8 MiB of bytes fits the 16 MiB bridge frame with the JSON envelope.
pub const DOCUMENT_LIMIT: u64 = 8 * 1024 * 1024;
pub const PAGE_LIMIT: u32 = 64 * 1024;
pub const CHUNK_LIMIT: u32 = 8 * 1024 * 1024;
pub const IMPORT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Bytes(#[serde(with = "base64_bytes")] pub Vec<u8>); // a small local serde module; crosses as base64 text

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permissions {
    Private,
    PrivateReadOnly,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadReply {
    pub data: Bytes,
    pub eof: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatKind {
    File,
    Directory,
    Other,
    Missing,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stat {
    pub kind: StatKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimReply {
    pub claim_id: String, // claimId on the wire
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

/// The current content (none when absent); the next content, no write, or a refusal.
pub type ChangeFn =
    Arc<dyn Fn(Option<Bytes>) -> BoxFuture<'static, Result<Option<Bytes>, Rejected>> + Send + Sync>;

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
    Io {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
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
    pub async fn add_root(&self, path: String, cancelled: CancelledFn) -> Result<String, FileError> {
        // ...
    }

    pub fn root_path(&self, root: String) -> Result<String, FileError> {
        // ...
    }

    pub async fn stat(&self, path: String, cancelled: CancelledFn) -> Result<Stat, FileError> {
        // ...
    }

    pub async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        length: Option<u32>,
        cancelled: CancelledFn,
    ) -> Result<Option<ReadReply>, FileError> {
        // ...
    }

    pub async fn claim(
        &self,
        caller: &Ctx,
        path: String,
        cancelled: CancelledFn,
    ) -> Result<ClaimReply, FileError> {
        // ...
    }

    pub async fn release(&self, caller: &Ctx, claim_id: String) -> Result<(), FileError> {
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
        keep_replaced_as: Option<String>,
        permissions: Option<Permissions>,
        cancelled: CancelledFn,
    ) -> Result<UpdateReply, FileError> {
        // ...
    }

    pub async fn update_claimed(
        &self,
        caller: &Ctx,
        path: String,
        change: ChangeFn,
        claim_id: String,
        keep_replaced_as: Option<String>,
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

`CancelledFn` is the crate's existing cancellation argument ([`host::CancelledFn`](../../../../crates/dim-agent-capabilities/src/host.rs)). `Bytes` crosses as base64 text; `Name` as a string or the native `{Unix: …}`/`{Windows: …}` object; root ids and sizes as strings and numbers. `claim`, `release` and `update_claimed` take `caller: &Ctx` because the claim is bound to the calling row; the dispatch fills it, and it is not a request field. Requests carry callbacks by reference only where the operation needs the caller's own step — the write source and the update change — plus the cancellation argument; every other argument and reply is data.

## Current implementation

The code at this head still exposes the lease surface that the contract above replaces. Where the two differ, the contract is authoritative and the delivery part on [#2016](https://github.com/arcships/dim/issues/2016) is the migration. The sections below record the behaviour that is not part of the contract: traversal, the account marker's wire format, creation, listing, removal and their tests.

### Events and lifetime

No Timeline events. Each `apply` opens fresh root-directory handles; blocking
operations and leases are tracked by that generation. Close stops admission,
drains operations/leases and releases resources. Retained old service handles
reject new operations. Clone the plugin to preserve append-only authorized-root
indices across reload; rebuilding from the original list could reuse indices.
Cancellation is checked before commit, not treated as rollback after installation.
Roots reject symlinked traversal; this is a trusted host storage boundary, not
isolation from the host or same-user processes. Do not automatically retry an
uncertain committed mutation. The native `open` used by each operation receives
exactly one already validated path component, never a multi-part path, so `..`
or an intermediate symlink cannot escape the held parent handle. Native Windows
opens leave `OBJECT_ATTRIBUTES.Attributes` at 0 so the actual directory's
case-sensitivity setting governs lookup; forcing `OBJ_CASE_INSENSITIVE` or
folding names would break case-sensitive directories and their aliases.

### The lease surface the contract replaces

The code's public `path`, `resolve`, `register_root`, `stat`, `lock`, `unlock`, `FileLease`, `read_limited`, `open_read`/`readAt`/`closeRead`, `replace`, `backup`, `import` with the old `PathBuf` source and required `limit`, `create_directories` and the `FsHost` wrapper are the surface the contract's `read`, `write`, `update`, `update_claimed`, `claim`, `release`, `stat`, `import` and `create_directory` replace; `add_root` and `root_path` stay on the face as named above. The code's `resolve` is this owner's path lookup; the file-tools face's `resolve` is a different operation under [#1297](https://github.com/arcships/dim/issues/1297). Their tests below stay until the migration lands.

### Lock keys

A lease locks a key file with the target's name in the reserved
`.dim-file-storage-locks` directory beside the target, so the filesystem's own
case and Unicode aliases of one name share one key. Releasing a lease keeps its
key; only directory removal unlinks keys, one at a time while holding each. A
lock counts only if its key is still linked once held. A key linked when locked
stays linked while held, because only its holder unlinks it. A key unlinked
before it was held is stale: the current key is resolved once more, and a
second stale key returns `Locked`. Without that check, a lock racing a failed
removal admitted a second holder:

```text
B: lock(d/x)              remove(d)                    C: lock(d/x)
open key x (inode 1)
                          lock, unlink every key,
                          rmdir the lock directory
                                                       recreate the lock directory,
                                                       create and lock key x (inode 2)
                          rmdir d fails, release keys
lock inode 1              -> before: B and C both hold d/x
                          -> now: inode 1 is unlinked, so B reopens key x,
                             finds inode 2 held and returns Locked
```

Creating a nonexclusive key creates it once and then opens that inode:
concurrent creation can return `ENOENT` on macOS even after the winner
created the key.

A replacement writes its temporary file in
`.dim-file-storage-locks/.dim-file-storage-temps` under its key's name, so only
that key's holder writes or removes it. A leftover from an interrupted
replacement is discarded by the next replacement of that name. `replace_from`
takes the replacement from a reader one 64 KiB chunk at a time with the same
temporary file, permissions, rename and durability rules, so a replacement
need not be held whole; a read that fails or is cancelled before the rename
removes the temporary file and leaves the target as it was.

`auth.json` also takes the TypeScript-compatible `auth.json.lock` marker, which
is reclaimed only while holding the `auth.json` key. A marker is stale once it
is older than 30 seconds and has no verified owner: its recorded PID must still
be running and have started no later than the marker's recorded creation time.
A reused PID, a permission-denied process probe or a missing PID or creation
time does not establish ownership. The exception is a marker this
process failed to release: it keeps the owner record, whose nonce only that
marker carries, and reclaims the marker at once, so removal and locking need
not wait for the process to exit. The marker is the published marker format: PID, milliseconds since the Unix
epoch, nonce and a trailing newline, with no other fields. Writers must use
milliseconds, not seconds.

The start-time comparison on macOS, Linux, Android and Windows assumes wall-clock
adjustments do not make the reported process start later than marker creation;
otherwise a live owner's marker can be reclaimed after the grace period. Other
Unix targets retain the marker conservatively while its PID probe succeeds.

On Windows, releasing a `PhysicalLock` unlocks its byte range explicitly before
release completes, because `CloseHandle` may defer the implicit unlock; a later
generation would otherwise see a stale `Locked` lock.

### Directory creation

The contract's `create_directory` makes the whole path an existing tree; the code below creates one level per call and `create_directories` composes the levels.

`create_directory` makes a path below an authorized root an existing business
directory, one level at a time: its parent must exist and is reached as every
operation reaches it, without following links. A missing directory is created
relative to the held parent handle, with mode `0700` on Unix (`mkdirat`) and
the parent's inherited DACL on Windows (`NtCreateFile` with `FILE_CREATE`). An
existing business directory counts as created, so a retry is safe; any other
existing entry (a file, link, reparse point or special file) fails with an I/O
error and is left untouched.

Aliases are checked the way `lock` checks them, before anything is created. The
parent's reserved directory is ensured first, so a name the filesystem equates
with it is recognized by identity. Then the key for `auth.json.lock` is ensured
and the key for the name is opened if it exists; if they are the same file, the
name is an alias of the account marker. Both fail `InvalidPath`. The name's key
is never created, so making a directory leaves no key behind in a parent that
may never be removed, such as SessionStore's Session-files directory.

```text
create_directory(d/x):
  open d (no links, never the reserved directory)       missing -> NotFound
  ensure d/.dim-file-storage-locks
  x is the reserved directory under another spelling?    -> InvalidPath
  ensure key auth.json.lock; key x exists and is it?     -> InvalidPath
  cancelled?                                             -> Cancelled
  mkdirat(d, x, 0700); an existing entry is kept
  x is not a business directory?                         -> Io, untouched
  fsync d                                                fails -> Unsynced
```

In both the created and the existing case the parent is synced before the call
returns, so a retry after `Unsynced` confirms durability. Windows has no
directory sync, as for replacement. Cancellation is checked up to the creation.

`create_directories` makes a multi-level path an existing directory tree by
calling that same one-level creation for each component, holding the opened
parent between levels, and returns the absolute path the path names below the
root. A component that already exists as a link, file or other non-directory
fails `InvalidPath` and is left untouched, and so does an alias of a reserved
resource; the returned path therefore names the result without a followed
link. This is the primitive for handing a work directory to a helper process.

### Listing and removal

The contract's `remove` waits for a held key; the code below answers `Locked`.

`list` returns the entries of a directory below an authorized root, sorted by
name, each a `File`, `Directory` or `Other` (a link, reparse point or special
file, never followed). FileStorage's lock, temporary and account-marker
resources are omitted, including names the filesystem equates with them.

`EntryKind` serializes as `file`, `directory` or `other` for generated exports.
Native entry names remain `OsString`; the exporting boundary chooses its UTF-8
policy, and one that returns a path-shaped name joins the resolved components
with `/`, so the name round-trips through its own path resolution: on Unix a
backslash is part of a name, and on Windows it is a separator.

`remove` makes an entry absent. Files, including those beneath a directory,
are removed under the lock leases use, so removing a leased file, or a directory
with a lease held anywhere beneath it, returns `Locked`. A link is removed
itself. A directory is emptied depth first through directory handles, without
following links or entering a filesystem mounted at or inside it (a Linux bind
mount of the same filesystem is not detected). An explicit stack replaces
recursion, so depth costs no thread stack. On Unix, removal holds one directory
descriptor at a time: it returns to each parent through `..` and fails if that
is no longer the directory it descended from, or starts again from the target if
a concurrent removal deleted it. Windows holds a handle per level, since it
cannot reopen a parent from a handle without a path.

Before emptying a directory, removal clears the leftovers that would stop
every file in it from being locked: a non-directory in place of the lock
directory or of the temporary directory inside it, and in the lock directory
any link, or any directory other than the temporary one, such as a directory
in place of a key. Keys are regular files and no `FilePath` reaches inside the lock
directory, so no lease holds these. The one exception is a regular file in
place of the temporary directory, which can be a key, so it goes under its
lock. Trees there are removed without locks, entering directories with
reserved names as well, but never another filesystem. The temporary
directory, however it is spelled, is never taken for such a tree: that is
checked on the handle the removal walks, so a temporary directory created
meanwhile is not either.

Then the directory's other leftovers go first: entries with a reserved name,
or with the identity of the lock directory or the account marker but another
type. Examples are a resource from an earlier version and a marker that is a
directory or a link. They follow the business rules, since on a
case-insensitive volume an alias of a reserved name can be leased. Going first
means a stray marker cannot block locking `auth.json` beside it. A file whose
key the filesystem equates with the temporary directory, such as a file named
`.dim-file-storage-temps`, or `.DIM-FILE-STORAGE-TEMPS` on a case-insensitive
volume, cannot be locked, so no lease holds it; it is unlinked without a lock
unless it is the account marker under another name. The directory's FileStorage resources go last and hold one
key at a time, however many names the directory has used:

```text
remove(d), per directory, before its entries:
  a non-directory in place of .dim-file-storage-locks  -> unlink it
  in .dim-file-storage-locks:
    a file in place of temps                           -> lock it as a key, unlink
    any link                                           -> unlink it
    any directory but temps                            -> remove it as a tree
remove(d), once d's other entries are gone:
  auth.json.lock: holding key auth.json, reclaim it if stale; live -> Locked
  for each key k in .dim-file-storage-locks:
    lock k                                             held -> Locked
    k unlinked by a concurrent removal?                -> skip it
    remove temps/k (a directory as a tree), unlink k, release k
  for each temps/t still left (written under a key taken since,
  or named at random by an earlier version): the same, under key t,
  or without a key if key t would be temps itself
  rmdir temps, rmdir .dim-file-storage-locks, rmdir d
                    an entry or key created meanwhile -> DirectoryNotEmpty
remove(p), once p is absent (also when it already was):
  fsync p's parent                                     fails -> Unsynced
```

A link or special file is unlinked only while it still is one. If a key or
file has replaced it since it was listed, removal fails instead. Windows
checks the type through the handle that deletes the entry; Unix checks it
just before unlinking, which narrows the race without closing it.

Locking and replacement never delete leftovers. A non-directory in place of
the lock or temporary directory, or a directory in place of a key or a
temporary file, makes `lock` or `replace` fail with an I/O error, without
following or writing through it, until the directory is removed.

On Windows the replacement's temporary file is created with the target ACL in
the create call, never under the maintenance directory's inherited ACL and
tightened afterwards. Native replacement does not merge alternate data streams,
so a target with a non-default stream fails explicitly. Its rename uses
`NtSetInformationFile` with the held parent handle and a basename, without
resolving an absolute path again, so a parent or symlink replaced after
validation cannot be followed.

Removal therefore never unlinks a key or temporary file that a holder has
locked. An absent entry counts as removed. Removal is not atomic: an error or
cancellation can leave part of a tree, retrying is safe, and an owner that must
not leak files after a crash reconciles by listing. Once the entry is absent,
removal syncs its parent before it returns, also when the entry was already
absent, so a completed removal survives a crash and a retry after `Unsynced`
confirms durability; an owner can order a commit after it, as SessionStore's
Session delete does ([S5c](../../../design/appserver/session-deletion-and-fork.md#4-crash-convergence)).
Windows has no directory sync, as for replacement. The
owner orders removal after the directory's writers; entries or leases created
meanwhile make it fail. A replacement racing the removal of its directory still
commits: if removal deleted the empty temporary directory first, the
replacement creates it again. Roots, the reserved directory and the account
marker cannot be listed or removed.

On Windows, every reparse point is listed as `Other` and never entered. Removal
deletes a junction or symbolic link itself, but refuses a volume mount point
(a mount-point reparse point whose target is a `\??\Volume{…}` path) with
`CrossesDevices`, as Unix refuses a directory on another filesystem; Unix lists
such a directory as `Directory`. Listing and removal need a volume that reports
file IDs during enumeration, such as NTFS or ReFS. Elsewhere, such as on FAT,
exFAT or some network shares, they fail with an `Unsupported` I/O error rather
than list entries whose aliases of reserved names could not be recognized.

A directory deleted while an operation runs inside it has the same effect on
both platforms. Unix fails an operation in a deleted directory, or on a deleted
entry, with not found. Windows fails it with access denied, as it does a real
denial (`STATUS_DELETE_PENDING` or `STATUS_FILE_DELETED`). FileStorage then
checks whether the handle's directory or entry is pending deletion and, if
so, reports not found. A real denial in a live directory stays an error. A
file deleted while a lock resolves its stored name counts as missing, rather
than taking the name NTFS gives deleted files, and a key that fails to lock
once a removal unlinked it is stale on either platform. A lock racing the
removal of its directory therefore resolves its key again or returns
`Locked`, or not found once the directory is gone. A removal racing
another removal of the same tree treats what the other deleted as removed,
and starts again from its target if a directory it returns to was deleted.

### Tests

[file_storage_flow.rs](../../../../crates/dim-agent-capabilities/tests/file_storage_flow.rs):
`late_roots_preserve_indices_and_generation_boundaries_on_reload`,
`file_storage_flow` (path, lock, permission and platform checks),
`directory_creation_stays_inside_the_authorized_root` (mode, an existing
directory, a missing parent, aliases of the reserved directory and the account
marker, an existing file or link, cancellation),
`streaming_replacement_and_nested_directories_stay_inside_the_authorized_root`
(replacement from a reader with a bounded read, a source that cancels part-way
leaving the old contents and no temporary file, nested creation with modes, an
existing directory, and a file or link at any level),
`directory_listing_and_removal_stay_inside_the_authorized_root` (Session-file
layout, reserved resources and aliases, leases held during partial removal,
links, cancellation, leftovers with reserved names or of the wrong type, a
leased alias of a reserved name, stray entries in the lock and temporary
directories, a marker whose release failed) and
`removal_scales_with_directory_size` (a directory that used 300 names and a
1,000-level tree, removed under a 64-descriptor limit on a 256 KiB worker
stack, where recursion overflowed at a few hundred levels). The unit
test `lock_keys_stay_exclusive_across_concurrent_directory_removal` in the
owner module drives the lock, removal and temporary-file interleavings that the
public API cannot produce reliably.

<!-- generated:plugin FileStorage — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | — |
| 提供 | `P:FileStorage`、`P:FsHost`、`P:dyn HostDispatch#fsHost` |
| 被注入于 | `HostPlatform` |
<!-- /generated -->
