# SqliteStorage

Scope: process. Issue: #2018.

SqliteStorage owns the database file, the lease that admits one process to it, the format version and its upgrades, the writer and the read connections, and the transactions other owners' records are read and written in. This document is the owner's target contract. The current implementation is described in [Native storage format](../appserver/native-storage-format.md) and [Resource owner operations](../contracts/resource-owner-operations.md).

## Use and scope: responsibility split

SqliteStorage owns:

- **The file and its lease.** `<db>` and `<db>.lock`; one process opens the store, upgrades it and holds the lock until close; an offline tool takes the same lock.
- **The format.** The version row, the schema, the ordered upgrades and the refusal of an unsupported format.
- **The connections.** One writer connection with its queue, a read pool, the WAL checkpoint and the close drain.
- **Transactions and reads.** One commit or one consistent read per call; the engine does not interpret the rows.
- **Offline operations.** Backup, restore and vacuum, each holding the same lease and refusing while a daemon owns the store.
- **The capacity value.** Database bytes, WAL bytes and reclaimable bytes.

The stored form of each group of rows belongs to a data owner: the Timeline batches and positions to the Session store's engine, the Agent records to the Agent directory, the Settings documents to Settings, the Provider selection and preferences to Provider. Each group's request and reply shape is that owner's persistence contract; this contract names the transactions and keeps their boundaries.

Callers keep:

- **The meaning of their rows.** Admission, versions, business conflicts and error mapping.
- **When to read or write.** The Session lifecycle, the Agent directory's startup repair, the Settings document's validation, the Provider's edit preconditions.
- **Their own lifecycle.** The engine never closes a caller's owner or decides a caller's business.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Admit and commit a Timeline batch round | No: one grouped transaction, the writer queue and the per-Session fallback are the engine's | `commitTimelineRound` and the Timeline operations |
| Read a Timeline position, session configuration or branch set | No: the caller cannot read the file | `head`, `batchEnd`, `batchEndAt`, `branches`, `sessionConfig`, `createSession` |
| Load, save or delete Agent records and their relations | No: the records are rows with one writer and one transaction | the Agent record group |
| Read, import and replace Settings documents | No: the version check and the document row are one transaction | the Settings group |
| Read and write Provider selection and preferences | No: the scope version and the selected model are one transaction | the Provider group |
| Report the store's own bytes | No: only the process holding the file knows them | `storageReport` |
| Read and write Session file ownership rows | No: they belong to SessionFiles | not on this face: SessionFiles' private row operations |

Every row above is No because the rows share one file, one writer and one close; the operations exist so callers do not hold the connection, the transaction or the file.

## Caller example

Agents loads its records and repairs unfinished ones at startup.

```rust
let agents = storage.agents(Empty {}).await?;
let owners = storage.owners(Empty {}).await?;
let repair = storage.unfinishedOwned(Empty {}).await?;
```

Settings replaces a document under its expected version.

```rust
let next = storage.replace_user(ReplaceUser {
    source, expected_version, decision,
}).await?;
```

SessionStore writes one Session creation through the engine's transaction.

```rust
storage.create_session(CreateSession { session_id, config }).await?;
```

## Own contract

### Dependencies

None. The assembly provides the store; deployment chooses its instance. Offline tools take the same file lease through the owner's functions, not through the service.

### Operations

Every operation takes one request object and answers one reply object. A transaction is not cancelled once it starts; a long batch round takes the caller's cancellation token and cancels before its commit. The engine's errors are per operation; the caller maps them to its business errors.

| Group | Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- | --- |
| Timeline | `createSession` | Prepare the stored Session record and the main branch | `{sessionId, config}` | `{}` | None | `storage` |
| Timeline | `sessionConfig`, `branches`, `head` | One stored read | `{sessionId}`, `{sessionId}`, `{key}` | `{config}`, `{branches}`, `{sequence}` | None | `storage` |
| Timeline | `batchEnd`, `batchEndAt` | Validate a boundary or return the containing batch's end | `{key, sequence}` | `{}` or `{sequence}` | None | `storage` |
| Timeline | `commitTimelineRound` | Admit and commit one ordered round of batches grouped by Session | `{batches}` | `{sessions:[{sessionId, result}]}` | Before the commit | `cancelled`, `storage` |
| Agent records | `agents`, `owners`, `members`, `ledMembers`, `ownedAgents`, `memberSessions`, `agentOwner`, `unfinishedOwned` | One consistent read of the directory's records | `{}`, or one id | the existing record shapes | None | `storage` |
| Agent records | `save`, `delete` | Write one Agent record or remove it | `{id, profile, default, owner?}`, `{id}` | `{}` | None | `storage` |
| Settings | `readUser` | One stored document and its version, or none | `{source}` | `{version, document}?` | None | `storage` |
| Settings | `importUserIfAbsent` | Insert a validated document and its provenance if absent; return the winning row | `{source, document, importedRevision}` | `{version, document}` | None | `storage` |
| Settings | `replaceUser` | Replace a document under its expected version | `{source, expectedVersion, decision}` | `{version}` | None | `missing`, `conflict`, `overflow`, `rejected`, `storage` |
| Provider | `readSelection`, `readPreferences` | One stored selection or preference | `{scope}`, `{providerId, modelId}` | the stored value or none | None | `storage` |
| Provider | `writeSelection` | Write the selected model under the scope's expected version | `{scope, expectedVersion, selection}` | the committed selection and version | None | `conflict`, `storage` |
| Capacity | `storageReport` | The store's own file bytes, WAL bytes and reclaimable bytes | `{}` | `{databaseBytes, walBytes, reclaimableBytes}` | None | `storage` |

The file-ownership row operations (`reserveFile`, `commitFile`, `abandonFile`, `ownsFile`, `ownsOutput`, `ownedPaths`, `registerOutput`) are `pub(crate)` operations of SessionFiles, not this face. The `storage.report` client method registers through `managementMethods` and calls `storageReport`; the registration point is not a second operation.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `storage` | The transaction or read failed; the caller reports its own failure and retries the business step when that is safe |
| `cancelled` | The round stopped before its commit; nothing was written by this round |
| `missing` | The document or record does not exist; the caller decides whether to create it |
| `conflict` | Another writer committed at a different version; re-read and decide |
| `overflow` | The version or the scope exceeded its bound; the caller reports it as a storage failure |
| `rejected` | A protected field was changed; the caller keeps the user's document and reports the field |

The engine does not answer `not_found`: a missing row is the data owner's business answer, expressed by that owner's operation.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Open, upgrade, verify | Open the file, take the lease, read and write the format, run the ordered upgrades in one transaction, start the writer and the read pool | Not applicable: assembly admits the daemon once | Not applicable | A failed open leaves the previous format, or the startup note names the newer store | File and lease by SqliteStorage; the process exits |
| `commitTimelineRound` | Admit the round, encode grouped by Session, commit one transaction per group, answer one outcome per distinct Session | Before the commit; nothing is written | The admitted round completes; the reply may be lost | Admission stops; the admitted round ends before close | Rows by SqliteStorage; publication by the Session store |
| Other writes | One transaction | None after it starts | The transaction completes; the reply may be lost | Admitted writes end before close | SqliteStorage |
| Reads | One consistent read on a pooled connection | None; the read is bounded | Nothing to release | Pending reads end `closed` | SqliteStorage |
| `storageReport` | Read the file sizes and the free-page count | None | Nothing to release | Pending calls end `closed` | SqliteStorage |
| Close | Refuse new work, wait for the running calls, close the readers, checkpoint, close the writer, release the lease | Not applicable | In-flight calls finish | The next process opens after the lease is free | SqliteStorage |
| Offline backup, restore, vacuum | Take the same lease, operate on the file, release | Not applicable; the tool runs alone | Not applicable | Refused while a daemon holds the lease | The offline tool and SqliteStorage share the lease |

## Exceptions with reasons

- Offline tools (`dim-migration`, backup, restore, vacuum) open the file under the same lease and write rows in the published format: an import or a repair runs while no daemon may own the store, and the format is the contract both sides implement. The lease is the exclusion; the format version is the gate.
- `storageReport` is a client method and a service operation: the management method is a registration point (ADR 002), and the service operation is the same read for plugins.
- The engine's operations are named after their data owners' records because those owners define the shapes; the engine owns the stored form and the transaction, and adds no business rule.

## Verification

Flow tests on the owner:

- Opening a newer format refuses and leaves the file unchanged; an upgrade is one transaction; a second process fails to open while the lease is held.
- `commitTimelineRound` commits one outcome per distinct Session, keeps the input order and answers an outcome for a failed group without dropping the others.
- `replaceUser` on a stale version answers `conflict`; a protected-field change answers `rejected` without writing; `importUserIfAbsent` returns the winning row.
- `writeSelection` on a stale scope answers `conflict`; `readPreferences` answers none for an absent preference.
- `storageReport` answers the file, WAL and free-page values; the client method returns the same values.
- Closing drains the running calls, checkpoints and releases the lease; an offline tool cannot open while the daemon owns it.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | SqliteStorage is one process part. It writes no Timeline fact of its own and is not a Capability. |
| §2 scopes | Process: the file, the lease and the transactions are shared across Sessions. |
| §3 orthogonality | Every transaction belongs to one data owner's records; the engine interprets none. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business rule. |
| §4 rule 5 | The grouped round keeps its commit and fallback order. |
| §4 rule 6 | No context input. |
| §4 rule 7 | Callers use these operations; no caller holds a connection or a transaction. |
| §4 rule 8 | No infrastructure change; requirements below. |
| §4 rule 9 | The engine publishes no Timeline fact; the round is the Session store's publication path. |
| §7.2 ids and values | Requests and replies are data; no connection, statement or transaction crosses. |
| §7.2 one public face | The service is marked on the SqliteStorage type under the existing `sqliteStorage` name; the hand-written dispatch adapter beside it is replaced. |
| §7.2 caller mechanisms | The writer queue, the read pool, the WAL and the drain stay inside the owner. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | No dependencies; other owners depend on the engine. |
| ADR 002 D4 | The path and the format are structural configuration; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **Settings (#2017), Provider (#2019), Agent directory (#2020): your group's contract.** The operations in the Settings, Provider and Agent record groups are your persistence contracts, executed by this engine. Your contracts own their request, reply and error shapes; this contract keeps the transactions and does not reinterpret them. A comment on each sub-issue states the split.
2. **SessionScope (#2025): the Timeline operations have one face.** The engine owns the Timeline operations; the Session-scoped `timelineStore` publishes the same engine per Session. `store.query` and `store.sessionConfig` read the same engine from the process side. The engine has one operation set; SessionScope's contract states which face owns which operation and removes the duplicate name.

## Binding appendix

The service is marked on the `SqliteStorage` type; the wire name stays `sqliteStorage`. The hand-written adapter type is deleted. `CancelledFn` is the binding's cancellation argument, used only by `commitTimelineRound`.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0}")]
    Storage(String),
    #[error("the round was cancelled")]
    Cancelled,
    #[error("{0}")]
    Missing(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Overflow(String),
    #[error("{0}: {1}")]
    Rejected(String, String),
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

    pub async fn commit_timeline_round(
        &self,
        caller: &Ctx,
        request: Round,
        cancelled: CancelledFn,
    ) -> Result<RoundReply, StorageError> {
        // ...
    }
}
```

Every operation takes one request object, one reply object and the caller's `Ctx`; only `commitTimelineRound` waits and takes `CancelledFn`.
