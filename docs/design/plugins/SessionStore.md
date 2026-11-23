# SessionStore

Scope: process. Issue: #2018.

SessionStore owns the stored Session: its rows, its grants, its reply language, its title revision, its worktree records, the exclusion that keeps those records consistent with Session creation, and the Timeline storage engine those rows are read through. This document is the owner's target contract. The current implementation is described in [Session rows §5.2](../resource-faces.md#52-session-rows) and [Native storage format](../appserver/native-storage-format.md). The Session-scoped `timelineStore` face is published by SessionScope and is not covered here; the requirement this contract raises for it is at the end.

## Use and scope: responsibility split

SessionStore owns:

- **The Session row set.** `agent_sessions`, `session_owners`, the main branch record, the catalog row and the Timeline rows that make a stored Session. Creation writes them in one transaction; deletion removes them in the owner's order.
- **Session grants.** The stored path, kind and mode set, with the rule that a read grant never downgrades a write grant.
- **The stored reply language.** One member of the stored configuration record.
- **Title revisions.** The revision counter and the stored title value, one writer at a time, announced after commit.
- **Worktree records.** A recorded directory, its last-use time and the Session associations that point into it.
- **The Session exclusion.** One lock that keeps creation, fork, deletion, association and worktree removal from observing each other's partial state.
- **The Timeline storage engine.** The admitted batch queue, the grouped commit, paging, snapshots and release, executed over the Session rows. `timelineStore` publishes the same engine per Session; the Session face belongs to SessionScope.

Callers keep:

- **Admission and lifecycle.** Who may create, when a Session closes before deletion, which protocol version a creation uses and which features it may carry.
- **Fork policy.** The source role, the cut and the rest point, the context boundary and the removal of `initialTitle` from the configuration text the caller supplies.
- **Grant meaning.** Which path a user allowed, and how a path is resolved and presented.
- **Title text.** SessionTitle decides the text, the operator's length rule and when to write.
- **Worktree policy.** ManagedWorktrees decides the root, task ids, Git calls, what a prune selects and whether an operator confirmed it.
- **Catalog enrichment.** Agents adds live sequence and closed state to the stored page.
- **Business errors above this owner.** A caller maps a stored-state refusal to its own business error.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Create an admitted Session and its optional worktree association | No: the rows, the owner row, the main branch, the catalog row and the association are one transaction; a partial write would publish a Session without its rows or an association without its Session | `create` |
| Close a Session, then remove its output directories and rows | No: output removal, visible rows and Timeline rows must run under the Session exclusion and in this order; a caller composing them could delete a row a concurrent creation still names | `delete` |
| Copy a source Session's rows at a cut with the source's exact configuration text | No: the copy is bounded transactions; parsing and re-serializing the text would change bytes | `fork` |
| Read a stored Session's creation origin and leader link for an attach reply | No: the caller has no row access | `creationRecord` |
| Read the Agent that owns a stored Session before its delete notice | No: the caller has no row access | `owner` |
| Ask whether an id has stored rows without loading a Session | No: the check spans five tables | `hasRecords` |
| Read a stored Session's configuration without loading the Session | No: the caller has no row access and must not parse a stored record | `sessionConfig` |
| Read the configuration text a fork copies | No: the text must be copied byte for byte | `sessionConfigText` |
| List stored Sessions as one page, filtered and ordered by the storage contract | No: the page is a consistent read over the catalog row, the Session row and the title | `catalog` |
| Read an unloaded Session's history | No: the caller has no Timeline row access | `query` |
| Read the Session's grants | No: the caller has no row access | `grants` |
| Add or remove one grant | No: the upsert rule that keeps a write grant is the owner's invariant | `addGrant`, `removeGrant` |
| Write the reply language into the stored configuration | No: the caller must not rewrite the stored record | `setLanguage` |
| Read the title and its revision | No: the caller has no row access | `title` |
| Write a title under its expected revision and announce the commit | No: the revision check, the single writer and the post-commit notice are the owner's | `setTitle` |
| Record a verified worktree directory and the Session's subtree | No: the directory record, the association and the last-use time are one commit | `associateWorktree` |
| List recorded worktree directories or their Session associations, or learn which stored Sessions have a workspace under a root | No: the caller has no row access | `worktrees`, `worktreeUsages`, `sessionsUnder` |
| Remove one recorded worktree together with its directory | No: the association and workspace checks, the physical removal and the record deletion must not interleave with a Session creation that would point into the removed root | `claimWorktree`, `settleWorktree` |

An operation exists only where its row says No.

## Caller example

Agents creates a Session; admission, protocol checks and the worktree creation happened before the call.

```rust
// `prepared` is the value ManagedWorktrees.create returned.
store.create(Create {
    id, config, agent_id, protocol_version, leader: None, origin, worktree: prepared,
}, cancelled).await?;
```

Agents forks and deletes; both mistakes are the caller's to fix, and a concurrent deletion is not an error the caller retries.

```rust
let source_text = store.sessionConfigText(ForkSource { id: source }, cancelled.clone()).await?;
store.fork(Fork { source, id: fork, at, config_text: source_text.config_text }, cancelled).await?;
let removed = store.delete(Delete { id }, cancelled).await?;
```

SessionTitle reads the revision, writes one revision later and announces the commit inside the owner.

```rust
let current = store.title(TitleRead { id }, cancelled.clone()).await?;
// A conflict means another writer committed a revision first; the title text is recomputed.
let next = store.setTitle(SetTitle { id, expected_revision: current.revision, title }, cancelled).await?;
```

ManagedWorktrees removes a directory: the claim holds the exclusion, Git removes the root, the settle deletes or restores the record.

```rust
let claim = store.claimWorktree(Directory { repository, root }, cancelled.clone()).await?;
match git_remove(&root, force, cancelled).await {
    Ok(()) => store.settleWorktree(Settle { claim: claim.claim_id, removed: true }, cancelled).await?,
    Err(error) => {
        store.settleWorktree(Settle { claim: claim.claim_id, removed: false }, CancellationToken::new()).await?;
        return Err(error);
    }
}
```

## Own contract

### Dependencies

The engine's data face `sqliteStorage` for every row read and write and for the Timeline batches. The file owner `sessionFiles` for the Session file steps a delete releases. Neither dependency crosses back into SessionStore. The assembly provides both; deployment chooses the instance.

### Operations

Every operation takes one request object and answers one reply object. Writes are one transaction or one ordered settlement and are not cancelled once they start: a dropped future does not undo committed rows. Reads take the caller's cancellation token where the read can wait on a long page or a long history.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `create` | Write one admitted Session and its optional worktree association | `{id, config, agentId, protocolVersion, leader?, origin, worktree?}` | `{}` | Before the transaction starts; a dropped future does not undo a commit | `invalid`, `workspace_missing`, `storage` |
| `delete` | Remove a closed Session's output, visible rows and Timeline rows | `{id}` | `{hadRows}` | Before the first step; after the first commit the steps run to their end | `storage` |
| `fork` | Copy a source's rows at a cut with the supplied configuration text | `{source, id, at, configText}` | `{}` | Before the first transaction | `invalid`, `storage` |
| `creationRecord` | One Session's origin and leader link | `{id}` | `{origin, leader?}` | None | `not_found`, `storage` |
| `owner` | The Agent that owns a stored Session | `{id}` | `{agentId?}` | None | `storage` |
| `hasRecords` | Whether an id has any stored row | `{id}` | `{exists}` | None | `storage` |
| `sessionConfig` | A stored Session's decoded configuration | `{id}` | `{config?}` | None | `not_found`, `storage` |
| `sessionConfigText` | A stored Session's configuration text, byte for byte | `{id}` | `{configText?}` | None | `not_found`, `storage` |
| `catalog` | One stored Session page | `{request, agent?, version, hosts, closed}` | the stored page | While the page is read | `invalid`, `cancelled`, `storage` |
| `query` | One consistent history read for an unloaded Session | `{scope, query, page?}` | the query result | While the page is read | `storage`, `cancelled` |
| `grants` | The Session's grant set | `{id}` | `{grants}` | None | `storage` |
| `addGrant` | Add one grant, never downgrading a write grant | `{id, path, kind, mode}` | `{}` | None | `not_found`, `invalid`, `storage` |
| `removeGrant` | Remove one grant; an absent grant counts as removed | `{id, path}` | `{}` | None | `storage` |
| `setLanguage` | Write the reply language into the stored configuration | `{id, language?}` | `{}` | None | `not_found`, `invalid`, `storage` |
| `title` | The title and its committed revision | `{id}` | `{revision, title?}` | None | `not_found`, `storage` |
| `setTitle` | Commit a title as the next revision and announce it | `{id, expectedRevision, title?}` | `{revision}` | The reply may be dropped; the admitted write is not cancelled | `not_found`, `conflict`, `storage` |
| `associateWorktree` | Record a verified directory and the Session's subtree in one commit | `{id, repository, root, subtree}` | `{}` | None | `not_found`, `invalid`, `storage` |
| `worktrees` | Recorded directories, with their last use when known | `{}` | `{directories}` | None | `storage` |
| `worktreeUsages` | The Session associations of one recorded directory | `{repository, root}` | `{usages}` | None | `invalid`, `storage` |
| `sessionsUnder` | Stored Sessions whose workspace is a root or lies under it | `{root}` | `{sessionIds}` | None | `invalid`, `storage` |
| `claimWorktree` | Claim one recorded directory for removal under the Session exclusion; the claim id is bound to the calling row | `{repository, root}` | `{claim}` | Before the claim is written | `not_found`, `in_use`, `invalid`, `storage` |
| `settleWorktree` | End a claim: delete the record when the directory is gone, restore it otherwise | `{claim, removed}` | `{}` | None; the owner completes the settlement | `unknown_claim`, `storage` |

`worktree` in `create` and `associateWorktree` is `{repository, root, subtree}`: absolute UTF-8 paths, `subtree` inside `root`. The caller passes paths it already resolved; the owner re-checks that each is absolute, UTF-8 and, for an association, that the root still is a directory.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the request: an id that is not one path component, a relative or non-UTF-8 path, a subtree outside the root, an unknown language code, an invalid catalog cursor |
| `workspace_missing` | The creation's `cwd` no longer is a directory; the caller reports the failed creation and discards the worktree |
| `not_found` | No stored Session has the id; the caller decides whether that is a business error or an answer |
| `conflict` | Another title write committed first; re-read `title` and decide |
| `in_use` | A Session association or a stored workspace still names the directory; delete that Session or refuse the removal |
| `unknown_claim` | The claim ended (the calling row unloaded, the owner reloaded, or another settlement ran); the directory state must be inspected before a retry |
| `cancelled` | The caller stopped the read; nothing changed |
| `closed` | This generation is unloading; call the replacement generation |
| `storage` | The store failed; the caller reports the failure and keeps the Session or worktree for a retry |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Validate the id and the workspace, take the exclusion, write the rows, the owner row, the main branch, the catalog row and the association, commit | Before the transaction: nothing written. During: the transaction completes or fails; the reply may be lost | The transaction still completes; the caller's row cannot use a Session it did not learn the id of, and a retry is a new id | New calls are refused `closed`; admitted work ends before the writer closes | Rows by SessionStore; the workspace by the caller |
| `delete` | Take the exclusion, read whether rows exist, remove output, touch worktree use, remove visible and Timeline rows, schedule the file sweep | Before the first commit: every row stays. After: the steps run to their end | The work continues under the owner's task; the reply may be lost | The admitted delete ends before close | Output and rows by SessionStore; files by SessionFiles |
| `fork` | Take the exclusion, copy in bounded transactions, write the target row with the supplied text, commit | Before the first copy | The copy continues; the reply may be lost | The admitted fork ends before close | Rows by SessionStore |
| `title`, `creationRecord`, `owner`, `hasRecords`, `sessionConfig`, `sessionConfigText`, `grants` | One read; no lock | Before the read | Nothing to release | Pending reads end `closed` | SessionStore |
| `catalog`, `query` | One consistent read under the caller's token | The read stops; no partial page | Nothing to release | Pending reads end `cancelled` | SessionStore |
| `addGrant`, `removeGrant`, `setLanguage`, `associateWorktree` | One write transaction | None after it starts | The write completes; the reply may be lost | Admitted writes end before close | SessionStore |
| `setTitle` | Admit the write under the owner's admission, run it as the owner's task, commit, announce, answer the revision | Dropping the reply never cancels the admitted write | The admitted write completes and announces | Close stops admission, waits for every admitted write | SessionStore |
| `claimWorktree` | Check associations and stored workspaces, mark the record claimed under the exclusion, bind the claim to the calling row | Before the mark | The owner restores the record and ends the claim | Claims are generation state: a reload drops them and restores the records | SessionStore |
| `settleWorktree` | End the claim; `removed` deletes the record, otherwise the record stays | None; the owner completes | Nothing to release | Pending settlements end `closed` | SessionStore; the directory by ManagedWorktrees |

## Exceptions with reasons

- `create` and `fork` take the caller's already-validated configuration and the fork text: validating the protocol, the collaboration role and the cut belongs to the Session lifecycle owner, and re-serializing the text would change bytes.
- `delete` does not close the Session, remove its Agent admission slot or remove the worktree: closing is Agents', and a managed worktree is removed by ManagedWorktrees under `claimWorktree`.
- `setTitle` hands the write over at admission: the reply may be lost, so the caller re-reads rather than assumes failure.
- `claimWorktree` and `settleWorktree` are two operations because the physical removal is ManagedWorktrees' resource: the claim is the store's exclusion state, not a handle; the id is bound to the calling row, so an unload restores the record instead of leaving it claimed.
- `owner` answers the Agent id for a delete notice; the Agent directory's own records are not returned here.

## Verification

Flow tests on the owner:

- `create` writes the Session, the owner, the main branch, the catalog row and the worktree association together; a failed association writes nothing.
- `delete` removes output first, then the rows; a failure before the first commit leaves every row; a concurrent creation cannot associate with a directory whose record is claimed.
- `fork` copies the supplied text unchanged and never parses it.
- `addGrant` on a missing Session answers `not_found`; `removeGrant` on an absent grant answers success; a read grant never downgrades a write grant.
- `setTitle` with a stale revision answers `conflict`; a dropped reply still commits and announces; close waits for the admitted write.
- `claimWorktree` answers `in_use` while an association or a stored workspace remains; a claimed directory refuses creation and association; an unloaded caller restores the record; `settleWorktree` with `removed: false` keeps the record.
- A call in flight when the generation closes ends `closed`; claims do not survive the generation.

Material uncertainty: the claim's lifetime when the daemon process dies between claim and settle is recorded only in memory; the record survives, the directory state is Git's, and a later `worktrees`/`remove` reconciles the pair. Review checks that no caller treats `unknown_claim` as success.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | SessionStore is one process part. It writes no Timeline fact and folds no state; it is not a Capability. |
| §2 scopes | Process: the Session rows outlive Sessions and are shared across them. Callers are process or Session parts. |
| §3 orthogonality | The Session rows and records are SessionStore's; other owners' rows are read through their own faces, not here. |
| §4 rule 1 | No command event: creation and deletion write no Timeline fact of their own. |
| §4 rule 2 | No handoff: the row set and the records stay with SessionStore. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | Core and assembly carry no Session policy; the contract has no protocol or feature rule. |
| §4 rule 5 | Deletion and fork keep their bounded transactions and their order. |
| §4 rule 6 | No context input. |
| §4 rule 7 | Callers use these operations; no caller reads a row, a writer generation or a lock. |
| §4 rule 8 | No infrastructure change; requirements are listed below. |
| §4 rule 9 | The store publishes no Timeline fact of its own; the attached worktree stays a process fact. |
| §7.2 ids and values | Requests and replies are data; the worktree value and the claim id cross, no object does. |
| §7.2 one public face | The service is marked on the SessionStore type under the existing `store` name; `Store`, `SessionMetadata` and `WorktreeAssociations` do not exist beside it. |
| §7.2 caller mechanisms | The exclusion, the title admission and the claim lifetime stay inside the owner; callers handle business errors only. |
| ADR 002 D1 | Process scope: the Session row set and its records outlive a Session. |
| ADR 002 D2 | No wiring plugin: one service on the owner type. |
| ADR 002 D3 | Dependencies point to the engine and the file owner; no caller holds a lower handle. |
| ADR 002 D4 | No business configuration. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **SessionScope (#2025): one spelling for the Timeline reads.** `store.query` and `store.sessionConfig` read the same engine as the Session-scoped `timelineStore`'s `query` and `sessionConfig`. The engine has one operation set; publish it once. The process read must stay reachable for an unloaded Session, so the contract asks SessionScope to state which face owns which operation and to remove the duplicate name. Probe: `store.query` and `timelineStore.query` return the same result for the same scope and through; a caller outside a Session uses the process operation.
2. **SessionScope (#2025): the `timelineStore` binding.** SessionStore constructs the engine and the Session-scope binding; the binding is assembly infrastructure. The contract assumes SessionScope owns the binding's installation and does not hand the binding to a process caller; a comment on #2025 states the requirement.

## Binding appendix

The service is marked on the `SessionStore` type; the wire name stays `store`. `Store` the wrapper, `SessionMetadata` and `WorktreeAssociations` no longer exist. `caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, so a claim id binds to the row without a request field. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::{PublicService, SessionConfig};
use dim_agent_types::grant::{GrantKind, GrantMode, SessionGrant};
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub repository: String,
    pub root: String,
    pub subtree: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub repository: String,
    pub root: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub claim: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settle {
    pub claim: String,
    pub removed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub session_id: String,
    pub subtree: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryRecord {
    pub repository: String,
    pub root: String,
    pub last_used_at_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Invalid(String),
    #[error("the Session workspace no longer exists: {0}")]
    WorkspaceMissing(String),
    #[error("no stored Session {0}")]
    NotFound(String),
    #[error("the title revision changed")]
    Conflict,
    #[error("{0} is in use")]
    InUse(String),
    #[error("the claim ended before settlement")]
    UnknownClaim,
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the store is unloading")]
    Closed,
    #[error("{0}")]
    Storage(String),
}

impl PublicService for SessionStore {}

#[rutis::service(name = "store")]
impl SessionStore {
    pub async fn create(
        &self,
        caller: &Ctx,
        request: Create,
        cancelled: CancelledFn,
    ) -> Result<(), StoreError> {
        // ...
    }

    pub async fn delete(
        &self,
        caller: &Ctx,
        request: Delete,
        cancelled: CancelledFn,
    ) -> Result<DeleteReply, StoreError> {
        // ...
    }

    pub async fn fork(
        &self,
        caller: &Ctx,
        request: Fork,
        cancelled: CancelledFn,
    ) -> Result<(), StoreError> {
        // ...
    }

    pub async fn claim_worktree(
        &self,
        caller: &Ctx,
        request: Directory,
        cancelled: CancelledFn,
    ) -> Result<Claim, StoreError> {
        // ...
    }

    pub async fn settle_worktree(
        &self,
        caller: &Ctx,
        request: Settle,
        cancelled: CancelledFn,
    ) -> Result<(), StoreError> {
        // ...
    }
}
```

The remaining operations follow the same shape: one request object, one reply object, the caller's `Ctx` where the operation is bound to the calling row, and `CancelledFn` only where the operation waits on a read.
