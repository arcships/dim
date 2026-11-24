# SessionStore

Scope: process. Issue: #2018.

SessionStore owns the stored Session: its rows, its grants, its reply language, its title revision, its worktree records, the exclusion that keeps those records consistent with Session creation, and the Timeline storage engine those rows are read through. This document is the owner's target contract. The current implementation is described in [Native storage format](../appserver/native-storage-format.md). The Session-scoped `timelineStore` face carries this owner's Timeline operations; SessionScope binds that face at the Session instance ([#2093](https://github.com/arcships/dim/pull/2093)).

## Use and scope: responsibility split

SessionStore owns:

- **The Session row set.** `agent_sessions`, `session_owners`, the main branch record, the catalog row and the Timeline rows that make a stored Session. Creation writes them in one transaction; deletion removes them in the owner's order. SessionStore writes `session_owners` inside its own create/delete transactions; Agents only reads it through `create`, `owner` and `hasRecords`, and its contract states the same split.
- **Session grants.** The stored path, kind and mode set, with the rule that a read grant never downgrades a write grant.
- **The stored reply language.** One member of the stored configuration record.
- **Title revisions.** The revision counter and the stored title value, one writer at a time, announced after commit.
- **Worktree records.** A recorded directory, its last-use time and the Session associations that point into it.
- **The Session exclusion.** One lock that keeps creation, fork, deletion, association and worktree removal from observing each other's partial state.
- **The Timeline storage engine.** The admitted batch queue, the grouped commit, paging, snapshots and release, executed over the Session rows. `timelineStore` publishes the same engine per Session; the Session face belongs to SessionScope.
- **The Session reads.** `sessionConfig {id}` and `query {scope, query, page?}` exist only on the process `store` and serve loaded and unloaded Sessions. `query` keeps the read scope: `scope` carries the Session key, the committed `through` cutoff and the branch segments, and `page.after` is checked against the same binding. No Session-scoped or `timelineStore` operation carries them.

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
| Copy a source Session's rows at a cut, deciding the target text from the source's exact text | No: the source body must be read and the target text decided under the fork's exclusion, and the target body and rows are written by the last batch transaction | `fork` with a text callback |
| Read a stored Session's creation origin and leader link for an attach reply | No: the caller has no row access | `creationRecord` |
| Read the Agent that owns a stored Session before its delete notice | No: the caller has no row access | `owner` |
| Ask whether an id has stored rows without loading a Session | No: the check spans five tables | `hasRecords` |
| Read a stored Session's configuration without loading the Session | No: the caller has no row access and must not parse a stored record | `sessionConfig` |
| List stored Sessions as one page, filtered and ordered by the storage contract | No: the page is a consistent read over the catalog row, the Session row and the title | `catalog` |
| Read a Session's stored history, loaded or not | No: the caller has no Timeline row access | `query` |
| Read the Session's grants | No: the caller has no row access | `grants` |
| Add or remove one grant | No: the upsert rule that keeps a write grant is the owner's invariant | `addGrant`, `removeGrant` |
| Write the reply language into the stored configuration | No: the caller must not rewrite the stored record | `setLanguage` |
| Read the title and its revision | No: the caller has no row access | `title` |
| Write a title and announce the commit | No: the write is serialized inside the owner, the decision reads the current title and revision under that serialization, and the committed notice is published after the commit | `setTitle` with a change callback |
| Record a verified worktree directory and the Session's subtree | No: the directory record, the association and the last-use time are one commit | `associateWorktree` |
| List recorded worktree directories or their Session associations, or learn which stored Sessions have a workspace under a root | No: the caller has no row access | `worktrees`, `worktreeUsages`, `sessionsUnder` |
| Remove one recorded worktree together with its directory | No: the association and workspace checks, the caller's own removal step and the record deletion must not interleave with a Session creation that would point into the removed root | `removeWorktree` with a removal callback |
| Remove an unregistered directory left by a failed creation | No: the stored-workspace check and the exclusion are the store's, and the directory has no record | `removeUnregisteredDirectory` with a removal callback |

An operation exists only where its row says No.

## Caller example

Agents creates a Session; admission, protocol checks and the worktree creation happened before the call.

```rust
// `prepared` is the value ManagedWorktrees.create returned.
store.create(ctx, CreateSession {
    id, config, agent_id, protocol_version, leader: None, origin,
    worktree: Some(Worktree { repository: prepared.repository, root: prepared.root, subtree: prepared.subtree }),
}).await?;
```

Agents forks and deletes; both mistakes are the caller's to fix, and a concurrent deletion is not an error the caller retries.

```rust
// The source text arrives in the callback; the initialTitle decision runs under the fork's exclusion.
let text: TextFn = Arc::new(move |source: String| {
    let next = strip_initial_title(&source); // lifecycle owner's decision
    Box::pin(async move { Ok(next) })
});
store.fork(ctx, ForkSession { source, id: fork, at }, text).await?;
let removed = store.delete(ctx, DeleteSession { id }).await?;
```

SessionTitle decides the next title from the current one; the owner serializes the decision and announces the committed revision.

```rust
let change: ChangeFn = Arc::new(move |current: TitleState| {
    let next = decide(current.revision, current.title.as_ref(), &text); // business rules
    Box::pin(async move { Ok(next) })
});
let next = store.setTitle(ctx, SetTitle { id }, change).await?;
```

ManagedWorktrees removes a directory: the owner holds the exclusion while the caller's Git step removes the root, and deletes or keeps the record by that step's result.

```rust
// The owner holds the Session exclusion and settles the record by this step's result.
let remove: RemoveFn = Arc::new(move |root: String| {
    let cancelled = cancelled.clone();
    Box::pin(async move { git_remove(&root, force, &cancelled).await })
});
store.removeWorktree(Directory { repository, root }, remove).await?;
```

## Own contract

### Dependencies

The engine's crate-private transaction operations for every row read and write and for the Timeline batches; the engine's public face is the capacity read. The file owner `sessionFiles` for the `releaseSession` step a delete calls. Neither dependency crosses back into SessionStore. The assembly provides both; deployment chooses the instance.

### Operations

Every operation takes one request object and answers one reply object. Writes are one transaction or one ordered settlement and are not cancelled once they start: a dropped future does not undo committed rows. Reads take the caller's cancellation token where the read can wait on a long page or a long history.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `create` | Write one admitted Session and its optional worktree association | `{id, config, agentId, protocolVersion, leader?, origin, worktree?}` | `{}` | Before the transaction starts; a dropped future does not undo a commit | `invalid`, `workspace_missing`, `rejected` (reason `exists`), `closed`, `storage` |
| `delete` | Remove a closed Session's output, visible rows and Timeline rows | `{id}` | `{hadRows}` | Before the first step; after the first commit the steps run to their end | `invalid`, `closed`, `storage` |
| `fork` | Copy a source's rows at a cut, with the target text decided from the source text under the fork's exclusion | `{source, id, at}` + `text` | `{}` | Before the first transaction; the callback's future is dropped | `invalid`, `not_found`, `rejected`, `closed`, `storage` |
| `creationRecord` | One Session's origin and leader link | `{id}` | `{origin, leader?}` | None | `not_found`, `closed`, `storage` |
| `owner` | The Agent that owns a stored Session | `{id}` | `{agentId?}` | None | `closed`, `storage` |
| `hasRecords` | Whether an id has any stored row | `{id}` | `{exists}` | None | `closed`, `storage` |
| `sessionConfig` | A stored Session's decoded configuration | `{id}` | `{config?}` | None | `not_found`, `closed`, `storage` |
| `catalog` | One stored Session page | `{request, agent?, version, hosts, closed}` | the stored page defined by the `sessions` service ([sessions service](../appserver/sessions-service.md)) | While the page is read | `invalid`, `cancelled`, `closed`, `storage` |
| `query` | One consistent history read of a Session's stored rows, for a caller outside the Session (loaded or not); `next` is the opaque cursor of the same read binding | `{scope, query, page?}`; `scope` is `{key:{sessionId,branchId}, through, segments:[{branchId, from, through, storedThrough, tail}]}`; `tail` is the producer's list of stored segment tails | `{result, next}` | While the page is read | `invalid`, `storage`, `cancelled`, `closed` |
| `grants` | The Session's grant set | `{id}` | `{grants}` | None | `closed`, `storage` |
| `addGrant` | Add one grant, never downgrading a write grant | `{id, path, kind, mode}` | `{}` | None | `not_found`, `invalid`, `closed`, `storage` |
| `removeGrant` | Remove one grant; an absent grant counts as removed | `{id, path}` | `{}` | None | `closed`, `storage` |
| `setLanguage` | Write the reply language into the stored configuration | `{id, language?}` | `{}` | None | `not_found`, `invalid`, `closed`, `storage` |
| `title` | The title, its source and its committed revision | `{id}` | `{revision, title?: {text, source}}` | None | `not_found`, `closed`, `storage` |
| `setTitle` | Commit the title the caller's change returns and announce it; the owner serializes writes | `{id}` + `change` | `{revision}` | The reply may be dropped; the admitted write is not cancelled | `not_found`, `rejected`, `closed`, `storage` |
| `associateWorktree` | Record a verified directory and the Session's subtree in one commit; the same Session+directory association replaces the subtree | `{id, repository, root, subtree}` | `{}` | None | `not_found`, `invalid`, `closed`, `storage` |
| `worktrees` | Recorded directories, with their last use when known | `{}` | `{directories}` | None | `closed`, `storage` |
| `worktreeUsages` | The Session associations of one recorded directory | `{repository, root}` | `{usages}` | None | `invalid`, `closed`, `storage` |
| `sessionsUnder` | Stored Sessions whose workspace is a root or lies under it | `{root}` | `{sessionIds}` | None | `invalid`, `closed`, `storage` |
| `removeWorktree` | Remove a directory under the Session exclusion, with the caller's own removal step; the record may be absent and an already-missing root is success | `{repository, root}` + `remove` | `{removed}`; true when a record was deleted | The caller's step ends; the record, if any, stays | `invalid`, `in_use`, `rejected`, `closed`, `storage` |
| `removeUnregisteredDirectory` | Remove a directory no record names, under the Session exclusion, with the caller's own removal step | `{root}` + `remove` | `{removed}` | The caller's step runs to its end; no record is involved | `invalid`, `not_found`, `in_use`, `rejected`, `closed`, `storage` |

`worktree` in `create` and `associateWorktree` is `{repository, root, subtree}`: absolute UTF-8 paths, `subtree` inside `root`, and no `.` or `..` component. The caller passes paths it already resolved; the owner re-checks that each is absolute, UTF-8, has no relative component and, for an association, that the root still is a directory.

#### Timeline face

`timelineStore` is the same owner's Session-scoped Timeline face; SessionScope installs the binding at the Session instance (#2093). This contract owns the spellings. The instance identity is the Session: every request's `sessionId` must equal it, and a mismatch answers `invalid`; that rule is part of every row's `invalid` below. The face carries the line reads and the completion subscriptions; `createSession`, `releaseTimeline`, `rollbackSession` and `append` are the store's own creation and commit steps, and `sessionConfig`/`query` are the process `store` reads.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `head` | A branch's last committed sequence | `{sessionId, branchId}` | `{sequence}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `branches` | The branch set with its heads | `{sessionId}` | `{branches:[{info, sequence}]}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `readPage` | One page of a branch's stored events | `{sessionId, branchId, after, limit, through}` | `{events}` | Before the reply | `invalid`, `not_found`, `cancelled`, `closed`, `storage` |
| `readSegments` | Several branch intervals in request order | `{sessionId, segments:[{branchId, from, through}]}` | `{segments}` | Before the reply | `invalid`, `not_found`, `cancelled`, `closed`, `storage` |
| `trace` | One stored trace at a position | `{sessionId, branchId, sequence}` | `{trace}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `batchEnd` | Validate a batch boundary | `{sessionId, branchId, sequence}` | `{}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `batchEndAt` | The containing batch's end sequence | `{sessionId, branchId, sequence}` | `{sequence}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `saveSnapshot` | Store an acceleration snapshot | `{sessionId, branchId, snapshot}` | `{}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `latestSnapshot` | The newest snapshot at or before a position | `{sessionId, branchId, through, interpreter}` | `{snapshot}` | None | `invalid`, `not_found`, `closed`, `storage` |
| `commits` | Register a completion listener for a Session | `{sessionId}` + `listener` | `{subscriptionId}` | The caller's unload withdraws it; an admitted batch is not replayed | `invalid`, `not_found`, `closed`, `storage` |
| `closeCommits` | Withdraw a completion listener; idempotent | `{subscriptionId}` | `{}` | None | `invalid`, `closed` |

`readPage`'s `limit` is a target event count; the final complete atomic batch may exceed it. `through` is an inclusive committed sequence; a read never returns unpublished events. `readSegments` replies in request order with `{event, trace, batchEnd}` records.

The completion notification is `{subscriptionId, batchId, branchId, through, result: "committed" | {code, message}}`, delivered after the batch is durable or has failed, in branch sequence order. `through` is the inclusive sequence of the batch's last event on `branchId`; it advances the durable watermark only for `"committed"`. A failed batch is reported once with its `batchId`, and later admissions for that Session are refused `storage` until the store releases it. A listener registered after a batch is admitted does not observe that batch; there is no replay. A slow observer never delays completion or release.

Flows:

- Admission is the store's internal step. A Session close waits for the store's release, which answers only after every admitted batch settled.
- The owner registers a listener with the calling row before returning; the row's unload withdraws it.
- `commits` and `closeCommits` on a closing generation answer `closed`; a pending read ends `closed`.

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `head`, `branches`, `trace`, `batchEnd`, `batchEndAt`, `latestSnapshot` | One read per call; no lock | Before the reply | Nothing to release | Pending reads end `closed`; a new call is refused `closed` | SessionStore |
| `readPage`, `readSegments` | Read the committed rows in request order | Before the reply; nothing changes | The read stops | Pending reads end `closed` | SessionStore |
| `saveSnapshot` | Write one snapshot row; the write is not cancelled once it starts | None; the caller may drop the reply | The write completes | Admitted writes end before close | SessionStore |
| `commits` | Register the listener with the calling row and answer the subscription id | The caller's unload withdraws it | The listener is withdrawn | A new call answers `closed`; registered listeners receive no further notifications | SessionStore |
| `closeCommits` | Withdraw the listener; idempotent | None | The listener is withdrawn | A new call answers `closed` | SessionStore |

After the store releases a Session, a face call answers `not_found`; after the generation closes, it answers `closed`. A failed batch refuses later admissions with `storage` but does not fail the reads.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the request: an id that is not one path component, a relative or non-UTF-8 path or a `.`/`..` component, a subtree outside the root, an unknown language code, an invalid catalog cursor. A `removeUnregisteredDirectory` root that lies under a registered directory answers `invalid`; call `removeWorktree` for it |
| `workspace_missing` | The creation's `cwd` no longer is a directory; the caller reports the failed creation and discards the worktree |
| `not_found` | No stored Session has the id or a fork source has no rows; the caller decides whether that is a business error or an answer. A worktree removal whose root is already gone is success: the step confirms the missing root and the record is deleted under the same exclusion, so a retry is safe (`crates/dim-app-server/src/storage/timeline.rs:1853-1890`) |
| `in_use` | A Session association or a stored workspace still names the directory; delete that Session or refuse the removal |
| `rejected` | The caller's change, text or removal step refused with its own `code` and `message`; nothing was written and the caller acts on that code. `create` with an id that already has rows answers reason `exists`; the caller reads the id instead of retrying |
| `cancelled` | The caller stopped the read; nothing changed |
| `closed` | This generation is unloading; call the replacement generation |
| `storage` | The store failed; the caller reports the failure and keeps the Session or worktree for a retry |

When a change, text or removal callback refuses, the operation answers `rejected` with the callback's own code in `reason` and its message unchanged; the wire code stays `rejected` so the callback's code cannot be mistaken for one of this owner's codes.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Validate the id, take the exclusion, validate the workspace, write the rows, the owner row, the main branch, the catalog row and the association, commit | Before the transaction: nothing written. During: the transaction completes or fails; the reply may be lost | The transaction still completes; the caller keeps the id it supplied and decides whether to publish or delete the Session | New calls are refused `closed`; admitted work ends before the writer closes | Rows by SessionStore; the workspace by the caller |
| `delete` | Take the exclusion, read whether rows exist, remove output, touch worktree use, remove the visible and Timeline rows and clear the leader link on member Sessions, then call `sessionFiles.releaseSession`, which deletes the file-ownership rows and schedules the bytes sweep; the store's own Timeline step is `releaseTimeline` | Dropping the caller before the handover: every row stays. After: the owner task runs to its end | The owner task continues; the reply may be lost | The admitted delete ends before close | Output and rows by SessionStore; files by SessionFiles |
| `fork` | Take the exclusion, read the source body, call `text`, copy in bounded transactions, write the target body and target rows in the last batch transaction, commit | Dropping the caller before the handover; a refused text step copies nothing | The owner task continues; the reply may be lost | The admitted fork ends before close | Rows by SessionStore |
| `title`, `creationRecord`, `owner`, `hasRecords`, `sessionConfig`, `grants` | One read; no lock | Before the read | Nothing to release | Pending reads end `closed` | SessionStore |
| `worktrees`, `worktreeUsages`, `sessionsUnder` | One read; no lock | Before the read | Nothing to release | Pending reads end `closed`; a new call is refused `closed` | SessionStore |
| `catalog`, `query` | One consistent read under the caller's token | The read stops; no partial page | Nothing to release | Pending reads end `cancelled`; a new call ends `closed` | SessionStore |
| `addGrant`, `removeGrant`, `setLanguage`, `associateWorktree` | One write transaction | None after it starts | The write completes; the reply may be lost | Admitted writes end before close | SessionStore |
| `setTitle` | Admit the write behind earlier admitted writes, read the current title, call `change` under the serialization, write the returned title as revision + 1, commit, announce, answer the revision | Dropping the reply never cancels the admitted write; a change that refuses leaves the title unchanged | Once the change is called the write commits even if the calling row unloads; a change not yet started is revoked and the write ends `closed` with the title unchanged | Close stops admission, waits for every admitted write | SessionStore; the change computes only |
| `removeWorktree` | Validate the paths, take the exclusion, check associations and stored workspaces, call `remove`, delete the record on success, release | The removal step refuses with its own code (for example `cancelled`); the record stays | The callback ends; the record stays; the exclusion releases | New calls are refused `closed`; an in-flight removal ends, and the record stays unless the step succeeded | Directory by the caller; record by SessionStore |
| `removeUnregisteredDirectory` | Validate the root, take the exclusion, check stored workspaces under it, run the removal step as the owner's task, release | The removal step refuses with its own code; nothing changes | The owner task continues to the end and the exclusion is not released to the caller | New calls are refused `closed`; the running removal finishes and keeps the lock until it ends | Directory by the caller; exclusion by SessionStore |

## Exceptions with reasons

- `create` takes the caller's already-validated configuration; `fork` reads the source text itself and calls the caller's text step under the fork's exclusion, so the lifecycle owner's `initialTitle` decision cannot race the copy and the copied bytes stay the source's text.
- `delete` does not close the Session, remove its Agent admission slot or remove the worktree: closing is Agents', and a managed worktree is removed by ManagedWorktrees under `removeWorktree`.
- `setTitle` runs the caller's change under the owner's serialization: the change only computes the next title from the current title and revision, and returns its own refusal; the reply may be lost, so the caller re-reads rather than assumes failure.
- `removeWorktree` serves both a recorded directory and a directory a failed creation left without a record (ManagedWorktrees' `discard`); the record is deleted only when it exists, and the reply says whether one was deleted. It runs ManagedWorktrees' removal step under the Session exclusion: the directory is that owner's resource, and the checks and the record deletion cannot interleave with a Session creation that would point into the removed root. The caller must not call SessionStore operations from the step; a failed or cancelled step keeps the record, and a row unload or owner close keeps it too. The exclusion has no time bound: a recursive forced deletion of an unregistered directory may run to its end, and `removeUnregisteredDirectory` uses the same exclusion for a directory no record names.
- `owner` answers the Agent id for a delete notice; the Agent directory's own records are not returned here.
- `associateWorktree` dedupes the directory record by `(repository, root)`: a repeated association for the same Session and directory replaces the subtree and updates its last use, and a new directory inserts one record. A retry after a lost reply therefore leaves one record and the caller's latest subtree.
- `setTitle`'s change runs inside the owner's serialization and must not call `store` operations that take the same lock; it computes the next title only.

## Client-visible changes

None. The client methods (`session.rename`, `sessions.list`, `session.delete`, `session.fork`) keep their request and reply shapes; the redesign changes the plugin surface only.

## Supersedes

- [Resource faces §5, §5.1, §5.2 and §9(6)](../resource-faces.md): the `store`/`timelineStore` split, the admission and drain steps, the Session rows, the Rust `Store` binding and the face names.
- [Resource faces §4.2](../resource-faces.md): the `associateWorktree` sentence.
- [Service faces §3.2, §3.3 and §3.4](../service-faces.md): the `worktreeAssociations` row, the `sessionStore` row and the `timelineStore` row.
- [Resource owner operations](../contracts/resource-owner-operations.md): the `sessionStore` row and the Native storage section.
- [Session title metadata](../appserver/session-title-metadata.md): §1 Write and Binding, §4.1, §4.2, §5, §6, §10 and Appendix A.
- [SessionTitle](SessionTitle.md): the metadata/notice row, the A.4 Write row and the Appendix G title metadata row.
- [Worktree access §4](../worktree-access.md): the `associateWorktree` sentence.
- [TypeScript plugin authoring](../../typescript-plugin-authoring.md): the title row (`sessionMetadata`, revision-checked `setTitle`).
- [Plugin contract](../plugin-contract.md): the `sessionMetadata` process-name row.
- [Session deletion and fork](../appserver/session-deletion-and-fork.md): the Worktree association port paragraph.
- [Overview](../overview.md): the SessionStore row.
- [Resource faces §1](../resource-faces.md): the `store` and `timelineStore` rows.
- [Service faces §4.8](../service-faces.md): the `associateWorktree` code block.

## Verification

Flow tests on the owner:

- `create` writes the Session, the owner, the main branch, the catalog row and the worktree association together; a failed association writes nothing.
- `delete` removes output first, then the rows; a failure before the first commit leaves every row; a concurrent creation waits for an in-flight removal and then either sees the surviving directory or fails on the missing workspace.
- `fork` calls the text step with the source text, copies the returned text unchanged, and a refused step copies nothing.
- `addGrant` on a missing Session answers `not_found`; `removeGrant` on an absent grant answers success; a read grant never downgrades a write grant.
- The change receives the current title, its source and revision; a change that returns `Rejected` leaves the title and revision unchanged; concurrent changes commit in admission order and each announces its revision; a dropped reply still commits and announces; close waits for the admitted write.
- `removeWorktree` answers `in_use` while an association or a stored workspace remains; a failed or cancelled removal keeps the record; a successful removal deletes it; a concurrent creation waits for the removal and then either associates with the surviving directory or fails on the missing workspace.
- A call in flight when the generation closes ends `closed`; a dropped callback keeps the record.
- A dropped `delete` or `fork` future still runs to its end on the owner task: the delete leaves rows and output consistent, and a fork never becomes visible half-copied.

Material uncertainty: a process death during the caller's removal step leaves the record; the directory state is Git's, and a later `worktrees`/`remove` reconciles the pair. Review checks that no caller treats `rejected` as success.

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
| §7.2 ids and values | Requests and replies are data; the title change and the worktree removal step cross as callbacks by reference. No object crosses. |
| §7.2 one public face | The `store` face is the owner's own hand-written `HostDispatch` on `SessionStore`; `Store`, `SessionMetadata` and `WorktreeAssociations` do not exist beside it. |
| §7.2 caller mechanisms | The exclusion, the title admission and the record settlement stay inside the owner; callers handle business errors only. |
| ADR 002 D1 | Process scope: the Session row set and its records outlive a Session. |
| ADR 002 D2 | No wiring plugin: one service on the owner type. |
| ADR 002 D3 | Dependencies point to the engine and the file owner; no caller holds a lower handle. |
| ADR 002 D4 | No business configuration. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **SessionTitle (#2033): the title write is a change callback.** `store.setTitle(id, change)` receives the current title, its source and revision, and returns the next title or its own refusal; the owner serializes the write and announces the commit. The revision-checked `{expectedRevision, title}` shape in SessionTitle.md and session-title-metadata.md is superseded; the manual-title rule moves into the change. A comment on #2033 states the requirement.

2. **SessionScope (#2025): `timelineStore` ownership aligned.** This owner owns the Timeline operations and the `timelineStore` face; SessionScope binds that face at the Session instance. The face does not carry `createSession`, `releaseTimeline`, `rollbackSession` or `append`; those belong to the store's own creation and commit paths. The process `store.query` and `store.sessionConfig` are the same engine's reads for callers outside a Session (loaded or not) and keep one implementation. A comment on #2025 records the alignment.


The delivery part maps the current codes to these kinds: `creationRecord`, `title` and `associateWorktree` move from `closed` to `not_found`; `addGrant` on a missing Session moves from `storage` to `not_found`; `setLanguage` moves from `closed` to `not_found`; `setTitle` during close moves from `storage` to `closed`; a worktree removal conflict moves from `invalid` to `in_use`; a missing `cwd` moves from `invalid` to `workspace_missing`; `delete` answers `hadRows` instead of `removed`. Field and operation renames: `sessionId`→`id`, `leaderSessionId`→`leader`, `createOwnedSession`→`create`, `worktreeDirectories`→`worktrees`, `sessionsUnderWorkspace`→`sessionsUnder`, `setSessionLanguage`→`setLanguage`.

## Binding appendix

The `store` face is one hand-written `HostDispatch` on `SessionStore`; the wire name stays `store`. `setTitle` is admitted in `invoke` before its reply future is returned, so a wire caller that drops the reply still commits. `Store` the wrapper, `SessionMetadata` and `WorktreeAssociations` no longer exist. `caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, so no caller sends or names a row. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::{PublicService, SessionConfig};
use std::future::Future;
use dim_agent_types::grant::{GrantKind, GrantMode, SessionGrant};
use rutis::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    Client,
    Acp,
    Internal,
    Automation,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub repository: String,
    pub root: String,
    pub subtree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    pub text: String,
    pub source: TitleSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource {
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleState {
    pub revision: u64,
    pub title: Option<Title>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSession {
    pub id: String,
    pub config: SessionConfig,
    pub agent_id: String,
    pub protocol_version: u32,
    pub leader: Option<String>,
    pub origin: SessionOrigin,
    pub worktree: Option<Worktree>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSession {
    pub id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionReply {
    pub had_rows: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkSession {
    pub source: String,
    pub id: String,
    pub at: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetTitle {
    pub id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleReply {
    pub revision: u64,
}

/// The caller's fork decision: the source's exact configuration text in, the target's text or its own refusal out.
pub type TextFn = Arc<
    dyn Fn(String) -> rutis::host::BoxFuture<'static, Result<String, StoreRejected>> + Send + Sync,
>;

/// The caller's decision: the current title and revision in, the next title (none clears) or its own refusal out.
pub type ChangeFn = Arc<
    dyn Fn(TitleState) -> rutis::host::BoxFuture<'static, Result<Option<Title>, StoreRejected>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub removed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub repository: String,
    pub root: String,
}

/// The caller's own refusal or failure, carried unchanged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct StoreRejected {
    pub code: String,
    pub message: String,
}

impl From<rutis::host::CallError> for StoreRejected {
    fn from(error: rutis::host::CallError) -> Self {
        let (message, raw) = error.into_raw();
        let code = raw
            .as_deref()
            .and_then(|raw| {
                raw.downcast_ref::<StoreRejected>()
                    .map(|rejected| rejected.code.as_str())
                    .or_else(|| {
                        raw.downcast_ref::<rutis_bridge::session::Error>()
                            .and_then(|error| error.code())
                    })
            })
            .unwrap_or("rejected")
            .to_owned();
        Self { code, message }
    }
}

/// The caller's own removal step: it removes the checked directory and reports success, or refuses with its own code and message.
pub type RemoveFn = Arc<
    dyn Fn(String) -> rutis::host::BoxFuture<'static, Result<(), StoreRejected>> + Send + Sync,
>;

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

/// The wire error: one variant per code; `code` is the variant name in snake_case.
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum StoreError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    WorkspaceMissing { message: String },
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    InUse { message: String },
    #[error("{reason}: {message}")]
    Rejected { reason: String, message: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the store is unloading")]
    Closed,
    #[error("{message}")]
    Storage { message: String },
}

impl PublicService for SessionStore {}

impl SessionStore {
    /// Admits the write under the owner's serialization and returns the wait future; dropping it never cancels the admitted write.
    /// `setTitle` stays on the hand-written method table: `invoke` calls this function eagerly and returns the wait, so a wire caller that drops the reply still commits the write.
    pub fn set_title(
        &self,
        caller: &Ctx,
        request: SetTitle,
        change: ChangeFn,
    ) -> impl Future<Output = Result<TitleReply, StoreError>> + Send + 'static {
        // admits the write, starts the owner's task, returns the wait
    }
}

/// The `store` face is a hand-written `HostDispatch` on the owner: one provider, one method table.
/// `invoke` decodes one request object, calls the method and returns the reply; `setTitle` calls
/// `set_title` eagerly before returning its wait, so a wire caller that drops the reply still
/// commits the write. The marked-service spelling of the other three owners does not apply here.
impl SessionStore {
    pub async fn create(
        &self,
        caller: &Ctx,
        request: CreateSession,
    ) -> Result<(), StoreError> {
        // ...
    }

    pub async fn delete(
        &self,
        caller: &Ctx,
        request: DeleteSession,
    ) -> Result<DeleteSessionReply, StoreError> {
        // ...
    }

    pub async fn fork(
        &self,
        caller: &Ctx,
        request: ForkSession,
        text: TextFn,
    ) -> Result<(), StoreError> {
        // ...
    }


    pub async fn remove_worktree(
        &self,
        caller: &Ctx,
        request: Directory,
        remove: RemoveFn,
    ) -> Result<Removed, StoreError> {
        // ...
    }
}
impl rutis_bridge::session::HostDispatch for SessionStore {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "create":"async", "delete":"async", "fork":"async", "creationRecord":"async",
            "owner":"async", "hasRecords":"async", "sessionConfig":"async", "catalog":"async",
            "query":"async", "grants":"async", "addGrant":"async", "removeGrant":"async",
            "setLanguage":"async", "title":"async", "setTitle":"async",
            "associateWorktree":"async", "worktrees":"async", "worktreeUsages":"async",
            "sessionsUnder":"async", "removeWorktree":"async",
            "removeUnregisteredDirectory":"async"
        }))
    }

    fn invoke(
        &self,
        method: &str,
        args: rutis_bridge::session::Value,
    ) -> rutis_bridge::session::Reply {
        // Decode one request object, call the method, return its reply. `setTitle` calls
        // `set_title` eagerly here, before the reply future is returned.
        let _ = (method, args);
        todo!()
    }
}
```

The remaining operations follow the same shape: one request object, one reply object, the caller's `Ctx` where the operation is bound to the calling row, and `CancelledFn` only where the operation waits on a read.
