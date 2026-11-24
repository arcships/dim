# ManagedWorktrees

Scope: process. Issue: #2018.

ManagedWorktrees owns the managed worktree root, the task ids under it, the Git worktree and branch a creation makes, and the selection a prune offers. This document is the owner's target contract. The current implementation is described in [Managed worktrees](../worktree-management.md). The Session association and the directory record belong to SessionStore; this owner asks the store for those operations.

## Use and scope: responsibility split

ManagedWorktrees owns:

- **The root.** `$DIMCODE_HOME/worktrees`, its canonical form and the rule that a managed directory is a plain directory under it.
- **Task ids.** Six lowercase base32 characters, unique under the root, allocated before the Session exists.
- **Git.** The worktree and branch a creation makes, the worktree registration, the removal and the prune.
- **The created value.** `{taskId, repository, root, subtree, branch, base}`: what a Session creation adopts or what a discard settles.
- **The preparation table.** Created but not yet adopted worktrees, by task id, for this generation.
- **Selection.** Which recorded directories a prune offers, which stay with a reason, and the re-check a removal repeats.

Callers keep:

- **When a Session starts in a new worktree.** `session.create` and `sessions.launch` decide; the caller passes the source workspace and the optional base and branch.
- **Session creation.** Writing the Session rows and the association is the store's `create`.
- **Operator confirmation.** The CLI shows the prune preview and sends the confirmed task ids.
- **Display.** The CLI renders the views and errors.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Make a managed worktree for a Session creation and learn the directory, subtree, branch and base | No: the root, the task id, the Git calls and the rollback are the owner's; a caller composing them would write a second creation path | `create` |
| Settle a created worktree that the Session creation did not adopt | No: the branch and base are the owner's, and the cleanup path must not be cancelled | `discard` |
| List managed worktrees with their branch, HEAD, content state and Session use | No: the view joins Git with the store's records and associations | `list` |
| Preview a prune: which directories are candidates and why the others stay | No: the age, keep and force rules and the re-checks are the owner's | `pruneSelection` |
| Remove one managed worktree | No: the removal needs the store's exclusion for the record and this owner's Git removal; the store's operation takes the Git step as the caller's own callback, so no lock or claim crosses | `remove` |
| Remove an unregistered directory left by a failed creation | No: the directory has no record; the owner decides whether it is a plain managed directory and whether its content allows removal | `remove` with `taskId` |

An operation exists only where its row says No.

## Caller example

Agents creates a worktree, writes the Session, and settles the worktree when the Session creation fails.

```rust
let created = worktrees.create(CreateWorktree { source, base, branch }, cancelled.clone()).await?;
match store.create(session_request(&created), cancelled.clone()).await {
    Ok(()) => (),
    Err(error) => {
        worktrees.discard(DiscardWorktree { task_id: created.task_id }).await?;
        return Err(error);
    }
}
```

The CLI previews a prune and removes the confirmed ids.

```rust
let preview = worktrees.prune_selection(PruneSelection { older_than_ms: Some(age), keep: None, force }).await?;
for candidate in confirmed(&preview) {
    worktrees.remove(RemoveWorktree { task_id: candidate.task_id, force }).await?;
}
```

## Own contract

### Dependencies

`store` for the directory records, the Session associations, the stored workspaces and `removeWorktree`, which runs this owner's Git step under the store's exclusion; `managementMethods` for the `worktree.*` client methods. Git is the owner's own process resource, started and reaped by this owner.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `create` | Allocate a task id and make a Git worktree and branch for a Session creation | `{source, base?, branch?}` | `{taskId, repository, root, subtree, branch, base}` | Between Git calls and before the branch is created; a cancelled creation removes its empty task directory | `worktree_unavailable`, `worktree_invalid`, `worktree_branch_exists`, `worktree_git_failed`, `worktree_interrupted` |
| `discard` | Settle a created worktree that the Session creation did not adopt | `{taskId}` | `{}` | Not cancellable: the cleanup runs to its end | `unknown_preparation`, `worktree_in_use`, `worktree_git_failed`, `worktree_interrupted`, `worktree_store_failed` |
| `list` | Managed worktrees with Git state and Session use | `{}` | `{worktrees}` | While the views are built | `worktree_unavailable`, `worktree_interrupted`, `worktree_store_failed` |
| `pruneSelection` | Candidates a prune would delete and the kept ones with reasons | `{olderThanMs?, keep?, force}` | `{candidates, kept}` | While the selection is built | `worktree_invalid`, `worktree_unavailable`, `worktree_interrupted`, `worktree_store_failed` |
| `remove` | Remove one managed worktree, or an unregistered directory, with its record; an unregistered removal keeps the branch and reports it | `{taskId, force}` | `{removed, keptBranch?}` | Before the Git removal; after it starts the removal completes or reports `worktree_git_failed` | `worktree_invalid`, `worktree_not_found`, `worktree_in_use`, `worktree_git_failed`, `worktree_interrupted`, `worktree_store_failed` |

`create` returns a value; `discard` and `remove` take the task id. No object crosses. The preparation table is the owner's generation state: a reload drops it, and `discard` for a task id the generation did not create answers `unknown_preparation`; the directory is then an unregistered directory that `remove` handles.

`remove` composes the store's removal under its exclusion: `removeWorktree` for a recorded directory and `removeUnregisteredDirectory` for a directory no record names. The store checks the associations and the stored workspaces and calls this owner's Git and filesystem removal as a callback; the record is deleted when the callback succeeds and kept when it refuses or fails. No claim id or lock crosses. `force` overrides only the content check; the Git removal receives `--force` only then.

### Errors

The wire codes are the published `worktree_*` codes (`crates/dim-app-server/src/worktrees.rs:85-108`); `unknown_preparation` is the only internal code.

| Kind | What the caller does next |
| --- | --- |
| `worktree_unavailable` | The daemon has no managed root or the source is not a usable Git worktree; the caller refuses the worktree request |
| `worktree_invalid` | Fix the request: an unknown task id form, both or neither of older-than and keep, a path that is not a plain managed directory |
| `worktree_branch_exists` | The requested branch already exists; choose another branch |
| `worktree_not_found` | No recorded or unregistered directory has the task id |
| `worktree_in_use` | A Session still uses the directory, or its content is not removable without `force`; the caller reports the blocker |
| `worktree_git_failed` | Git refused the operation; the caller reports the message and keeps the directory and the report |
| `worktree_interrupted` | The call was stopped or the generation closed; a creation's partial state is cleaned, a removal is retried on the replacement generation |
| `unknown_preparation` | The preparation ended with the generation; the directory is unregistered and `remove` handles it |
| `worktree_store_failed` | A record read or write failed; the caller reports the failure and keeps the record |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Resolve the source and the Git roots, resolve the base, allocate a task id, `git worktree add -b`, canonicalize the root, check the subtree, return the value; the caller adopts it with `store.create` | Each Git call ends `interrupted`; an empty task directory this call made is removed; a created worktree is rolled back by the owner | The call ends `interrupted`; the rollback path runs | New calls are refused; an in-flight creation is rolled back or left as an unregistered directory | Git and the directory by ManagedWorktrees; the record by the store |
| `discard` | Look up the preparation, `store.removeWorktree(directory, remove)`, delete the branch; the record may be absent | No cancellation; the cleanup is the owner's | The preparation ends with the row; the directory is unregistered | The table drops with the generation; the directory stays unregistered | Directory and branch by ManagedWorktrees; record by the store |
| `list` | Read the records and associations, join Git state per directory, add unregistered directories | Before the reply | Nothing to release | Pending calls end `closed` | ManagedWorktrees |
| `pruneSelection` | `list`, then apply age, keep and force rules per candidate | Before the reply | Nothing to release | Pending calls end `closed` | ManagedWorktrees |
| `remove` | Resolve the record or the unregistered directory; a recorded one goes through `store.removeWorktree(directory, remove)`, an unregistered one through `store.removeUnregisteredDirectory(root, remove)`: the store checks and holds the exclusion, this owner removes the Git worktree or the directory, the store deletes or keeps the record; a branch is never deleted on an unregistered removal and its name is answered in `keptBranch` | Before the Git removal; a started removal completes or reports `worktree_git_failed`; the store keeps the record | The callback ends with the row; the store keeps the record | An in-flight removal ends; the store keeps the record unless the Git step succeeded | Directory by ManagedWorktrees; record by the store |

A process death between `create` and `store.create` leaves an unregistered directory; `list` reports it and `remove` deletes it after the operator names the task id. A process death during the removal step leaves the record; the next `list` reports the directory as missing or no longer registered, and `remove` cleans the record.

## Exceptions with reasons

- A branch is kept when `remove` deletes an adopted worktree: committed content must not disappear with the directory. Only the default `dim/<taskId>` branch of a never-adopted worktree is deleted with the directory.
- `discard` is not cancellable: a cancelled Session creation still has to settle what the owner made. A store refusal (`closed` or `storage`) leaves the directory and branch in place; the caller reports it. The preparation is dropped with the generation, so the directory is unregistered and `remove` by task id is the retry path on the replacement generation.
- Unregistered directories are never pruned: the owner has no record to age, so only an explicit `remove` by task id deletes them.
- The preparation table exists because a created worktree has no record until the Session adopts it: the task id is the only handle a caller keeps after its own state is lost or reloaded, so `discard` settles by id instead of by a value the caller may no longer hold.
- A branch outlives a creation that was never adopted: a process death between `create` and `store.create` leaves the branch. `remove` on an unregistered directory never deletes a branch; it answers the branch the directory was on in `keptBranch`, and the operator deletes it with Git. The preparation table is gone by then, so the owner cannot tell a default `dim/<taskId>` branch from one the operator named; never deleting is the only decidable rule.
- Git is the owner's process resource: the owner starts each call, bounds it (10s read, 30s write) and reaps it on cancellation.

## Client-visible changes

None. `worktree.list`, `worktree.remove` and `worktree.prune` keep their request, reply and error shapes; the redesign changes the plugin surface and the owner name only.

## Supersedes

- [Managed worktrees](../worktree-management.md) §5: the removal paragraph is replaced by a pointer to this contract.

## Verification

Flow tests on the owner:

- `create` makes a worktree and branch under the root; a taken task id or branch retries with a new task id; an explicit existing branch answers `branch_exists`; a cancelled creation leaves no created worktree and no empty task directory.
- `discard` removes a created worktree that no Session adopted; a Session association or a stored workspace keeps it and answers `in_use`; `unknown_preparation` after a generation change.
- `remove` deletes a recorded directory and its record; a directory with content refuses without `force`; a missing root deletes the record only; an unregistered directory with content refuses without `force`; a link is refused.
- `pruneSelection` never selects an unregistered, unknown, used or unremovable directory; `keep` treats an unknown use time as oldest; `olderThanMs` never selects an unknown time.
- A failed or cancelled Git removal keeps the record; a row unload or owner close keeps the record; a successful removal deletes it.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ManagedWorktrees is one process part. It writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: the root, task ids and Git registrations outlive Sessions. |
| §3 orthogonality | The root and Git are this owner's; records are read through the store's operations. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No Session policy in this owner; the caller decides when to create. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context input. |
| §4 rule 7 | Callers use these operations; no caller invokes Git or reads the root directly. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact; a worktree is a process fact. |
| §7.2 ids and values | Task ids, paths and policies cross as data; the created value is data; the removal step crosses as a callback by reference; no object or claim id crosses. |
| §7.2 one public face | The service is marked on the ManagedWorktrees type under the existing `managedWorktrees` name; the client methods `worktree.*` are registration points, not a second face. |
| §7.2 caller mechanisms | The root, Git, the preparation table and the record settlement through the store's operation stay inside the owners; callers handle the business errors only. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin; the `worktree.*` rows register methods, they do not own state. |
| ADR 002 D3 | Depends on the store for records; no lower handle. |
| ADR 002 D4 | The root is structural configuration; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None new. The store's `removeWorktree` is defined in [SessionStore's contract](SessionStore.md); the `managementMethods` registration point is the existing mechanism.

## Binding appendix

The service is marked on the owner type; the wire name stays `managedWorktrees`. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub task_id: String,
    pub repository: String,
    pub root: String,
    pub subtree: String,
    pub branch: String,
    pub base: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardWorktree {
    pub task_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorktree {
    pub task_id: String,
    pub force: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorktree {
    pub source: String,
    pub base: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PruneSelection {
    pub older_than_ms: Option<u64>,
    pub keep: Option<usize>,
    pub force: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorktreeReply {
    pub removed: bool,
}

/// The wire error: one variant per code; `code` is the variant name in snake_case.
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WorktreeError {
    #[error("{message}")]
    WorktreeUnavailable { message: String },
    #[error("{message}")]
    WorktreeInvalid { message: String },
    #[error("{message}")]
    WorktreeBranchExists { message: String },
    #[error("{message}")]
    WorktreeNotFound { message: String },
    #[error("{message}")]
    WorktreeInUse { message: String },
    #[error("{message}")]
    WorktreeGitFailed { message: String },
    #[error("{message}")]
    WorktreeInterrupted { message: String },
    #[error("{message}")]
    UnknownPreparation { message: String },
    #[error("{message}")]
    WorktreeStoreFailed { message: String },
}

impl PublicService for ManagedWorktrees {}

#[rutis::service(name = "managedWorktrees")]
impl ManagedWorktrees {
    pub async fn create(
        &self,
        caller: &Ctx,
        request: CreateWorktree,
        cancelled: CancelledFn,
    ) -> Result<CreatedWorktree, WorktreeError> {
        // ...
    }

    pub async fn discard(
        &self,
        caller: &Ctx,
        request: DiscardWorktree,
    ) -> Result<(), WorktreeError> {
        // ...
    }

    pub async fn remove(
        &self,
        caller: &Ctx,
        request: RemoveWorktree,
        cancelled: CancelledFn,
    ) -> Result<RemoveWorktreeReply, WorktreeError> {
        // ...
    }
}
```

`list` and `pruneSelection` follow the same shape, with the views and the prune selection as reply data.
