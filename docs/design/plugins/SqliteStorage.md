# SqliteStorage

Scope: process. Issue: #2018.

SqliteStorage owns the database file, the lease that admits one process to it, the format version and its upgrades, the writer and the read connections, and the transactions the data owners' rows are read and written in. This document is the owner's target contract. The current implementation is described in [Native storage format](../appserver/native-storage-format.md).

## Use and scope: responsibility split

SqliteStorage owns:

- **The file and its lease.** `<db>` and `<db>.lock`; one process opens the store, upgrades it and holds the lock until close; backup and vacuum take the same lock, and restore writes a destination that must not exist.
- **The format.** The version row, the schema, the ordered upgrades and the refusal of an unsupported format.
- **The connections.** One writer connection with its queue, a read pool, the WAL checkpoint and the close drain.
- **Transactions and reads.** One commit or one consistent read per call; the engine does not interpret the rows. These operations are the engine's private implementation for the data owners; they are not a service face.
- **Offline operations.** Backup and vacuum hold the same lease and refuse while a daemon owns the store; restore writes a new destination that must not exist yet, and its exclusion is that destination rule.
- **The capacity value.** Database bytes, WAL bytes and reclaimable bytes, read through `storageReport`.

The rows belong to their data owners, and their operations live on those owners' faces: the Session rows and the Timeline batches to [SessionStore](SessionStore.md), the Agent records to Agents and Collaboration ([#2098](https://github.com/arcships/dim/pull/2098)), the Settings documents to Settings ([#2094](https://github.com/arcships/dim/pull/2094)), the Provider records to Provider ([#2096](https://github.com/arcships/dim/pull/2096)). The engine executes each owner's transactions and returns the stored form; it answers no business read or write on its public face and is not in the catalog for those rows.

Callers keep:

- **The meaning of their rows.** Admission, versions, business conflicts and error mapping.
- **When to read or write.** The Session lifecycle, the Agent directory's startup repair, the Settings document's validation, the Provider's edit preconditions.
- **Their own lifecycle.** The engine never closes a caller's owner or decides a caller's business.

| Requirement (public callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Report the store's own bytes | No: only the process holding the file knows them | `storageReport` |
| Back up or vacuum the store while no daemon owns it | No: the operation needs the file lease and the format check | `backup`, `vacuum` |
| Restore an archive into a new store | No: the destination rule and the format check are the owner's | `restore` |
| Bind the daemon's endpoints after the database lock is held | No operation: the dependency orders loading | the `sqliteStorage` key in `injects` |

The transaction operations the data owners use are not on this face: Settings, Provider, Agents, Collaboration and SessionStore call them as crate-private operations of the engine, and each owner's contract owns the request, reply and error shapes. A caller that needs different data asks the row's owner, not the engine.

## Caller example

The client method `storage.report` reads the capacity value; the management row registers the method and calls the same operation.

```rust
let report = storage.storageReport(Empty {}).await?;
// { database_bytes, wal_bytes, reclaimable_bytes }
```

The daemon's CLI runs an offline backup or vacuum while no daemon owns the file.

```rust
storage::backup_store(database.into(), destination.into())?;
```

## Own contract

### Dependencies

None. The assembly provides the store; deployment chooses its instance. Offline tools take the same file lease through the owner's functions, not through the service. The data owners are the engine's crate-private users, not service callers.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `storageReport` | The store's own file bytes, WAL bytes and reclaimable bytes | `{}` | `{databaseBytes, walBytes, reclaimableBytes}` | None | `closed`, `storage` |
| `backup` | Copy a closed store and its sidecar files into an archive | the database path and the destination | `{}` | None; the tool runs alone | `owned`, `format`, `storage` |
| `restore` | Write a store from an archive produced by `backup` into a destination that does not exist | the archive and the destination | `{}` | None; the tool runs alone | `format`, `storage` |
| `vacuum` | Remove the content rows no stored reference reaches and reclaim the file's free pages | the database path | the removed rows, content bytes and sizes | None; the tool runs alone | `owned`, `format`, `storage` |

`storage.report` is the client method registered through `managementMethods`; the registration point calls `storageReport` and is not a second operation. The transaction operations the data owners use are private to the storage crate and are listed in their owners' contracts.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `storage` | The read or the offline operation failed; the caller reports its own failure |
| `owned` | Another process holds the file's lease; wait for it to exit and retry the offline operation |
| `format` | The file is not the current format; open it with the daemon that upgrades it, or with a matching version |

The engine answers no business error: a missing row, a stale version or a refused change is the data owner's, expressed by that owner's operation.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Open, upgrade, verify | Open the file, take the lease, read and write the format, run the ordered upgrades in one transaction, start the writer and the read pool | Not applicable: assembly admits the daemon once | Not applicable | A failed open leaves the previous format, or the startup note names the newer store | File and lease by SqliteStorage; the process exits |
| `storageReport` | Read the file sizes and the free-page count | None | Nothing to release | Pending calls end `closed` | SqliteStorage |
| `backup`, `vacuum` | Take the same lease, operate on the file, release | Not applicable; the tool runs alone | Not applicable | Refused while a daemon holds the lease | The offline tool and SqliteStorage share the lease |
| `restore` | Require a destination that does not exist, write the archive there, verify the format | Not applicable; the tool runs alone | Not applicable | A destination a daemon owns is refused by the non-existence rule | The offline tool |
| Close | Refuse new work, wait for the running calls, close the readers, checkpoint, close the writer, release the lease | Not applicable | In-flight calls finish | The next process opens after the lease is free | SqliteStorage |
| Private transactions | The data owner's call runs on the writer or a pooled reader, under the open store's drain | The owner's operation decides its own cancellation | The writer completes or rolls back the transaction | The admitted calls end before close | SqliteStorage; the owner's contract owns the operation |

## Exceptions with reasons

- Offline tools (`dim-migration`, backup, restore, vacuum) write the published format while no daemon owns the store: backup, migration and vacuum take the same lease; restore writes a destination that must not exist. The format version is the gate for every one.
- `storageReport` is a client method and a service operation: the management method is a registration point (ADR 002), and the service operation is the same read for plugins.
- The data owners' transaction operations are crate-private: each owner's contract defines its request, reply and errors, and the engine executes them without interpreting the rows. Publishing them again under `sqliteStorage` would answer the owners' business on the engine's face.

## Verification

Flow tests on the owner:

- Opening a newer format refuses and leaves the file unchanged; an upgrade is one transaction; a second process fails to open while the lease is held.
- `storageReport` answers the file, WAL and free-page values; the client method returns the same values.
- Closing drains the running owner calls, checkpoints and releases the lease; an offline tool cannot open while the daemon owns it.
- A backup round-trips through restore; backup of a store whose format differs answers `format`; restore refuses an existing destination; vacuum refuses a store a daemon owns and a store whose format differs.
- The data owners' transactions are tested by their owners' flow tests; the engine's tests cover their commit, rollback and drain, not their business answers.

## Client-visible changes

None. The `storage.report` client method keeps its request, reply and error shapes; the offline CLI commands are unchanged.

## Supersedes

- [Resource owner operations](../contracts/resource-owner-operations.md): the Native storage section.
- [Resource faces §6](../resource-faces.md): the `sqliteStorage` row.
- [Service faces §3.4](../service-faces.md): the `sqliteStorage` row.
- [Native storage format](../appserver/native-storage-format.md): the SQLite owner operations note.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | SqliteStorage is one process part. It writes no Timeline fact of its own and is not a Capability. |
| §2 scopes | Process: the file, the lease and the transactions are shared across Sessions. |
| §3 orthogonality | Every transaction belongs to one data owner's rows; the engine interprets none. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business rule. |
| §4 rule 5 | The grouped round keeps its commit and fallback order; the round's operation is the Session store's. |
| §4 rule 6 | No context input. |
| §4 rule 7 | The data owners use the engine's private operations; no caller holds a connection or a transaction. |
| §4 rule 8 | No infrastructure change; requirements below. |
| §4 rule 9 | The engine publishes no Timeline fact; the round is the Session store's publication path. |
| §7.2 ids and values | Requests and replies are data; no connection, statement or transaction crosses. |
| §7.2 one public face | The service is marked on the SqliteStorage type under the existing `sqliteStorage` name, with the capacity read only; the hand-written dispatch adapter beside it is replaced. |
| §7.2 no proxy query | The engine does not answer the data owners' business reads or writes on its face; each row's owner answers them. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | No public dependencies; the data owners use the engine privately. |
| ADR 002 D4 | The path and the format are structural configuration; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **Settings (#2094), Provider (#2096), Agents and Collaboration (#2098): your operations own the rows.** The Settings document operations, the Provider record operations and the Agent record operations are defined and answered by their owners' contracts. The engine is their crate-private implementation: it executes the transactions and returns the stored form, and no longer publishes these operations under `sqliteStorage`. A comment on each sub-issue records the split.
2. **SessionScope (#2025): `timelineStore` ownership.** The Session store owns the Timeline operations and the `timelineStore` face; SessionScope binds that face at the Session instance. The face does not carry `createSession`, `releaseSession`, `rollbackSession` or `append`; those belong to the store's creation and commit paths. The process `store.query` and `store.sessionConfig` are the same engine's reads for callers outside a Session.

## Binding appendix

The service is marked on the `SqliteStorage` type; the wire name stays `sqliteStorage`. The hand-written adapter type is deleted. Only the capacity read is a service operation; the offline functions stay the owner's Rust functions, and the data owners' transactions are crate-private.

```rust
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub reclaimable_bytes: u64,
}

/// The wire error: one variant per code; `code` is the variant name in snake_case.
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum StorageError {
    #[error("{message}")]
    Storage { message: String },
    #[error("{message}")]
    Owned { message: String },
    #[error("{message}")]
    Format { message: String },
    #[error("the store is unloading")]
    Closed,
}

impl PublicService for SqliteStorage {}

#[rutis::service(name = "sqliteStorage")]
impl SqliteStorage {
    pub async fn storage_report(
        &self,
        caller: &Ctx,
        request: Empty,
    ) -> Result<Capacity, StorageError> {
        // ...
    }
}
```

`backup_store`, `restore_store` and `vacuum_store` remain the owner's functions with the signatures the CLI and the migration tool use; they take the same lease and are not service operations.
