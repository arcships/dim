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
| Remove one managed worktree | No: the removal needs the store's exclusion for the record and the owner's Git removal; a caller composing the two would hold the exclusion itself | `remove` |
| Remove an unregistered directory left by a failed creation | No: the directory has no record; the owner decides whether it is a plain managed directory and whether its content allows removal | `remove` with `taskId` |

An operation exists only where its row says No.

## Caller example

Agents creates a worktree, writes the Session, and settles the worktree when the Session creation fails.

```rust
let created = worktrees.create(Create { source, base, branch }, cancelled.clone()).await?;
match store.create(session_request(&created), cancelled.clone()).await {
    Ok(()) => (),
    Err(error) => {
        worktrees.discard(Discard { task_id: created.task_id }, CancellationToken::new()).await?;
        return Err(error);
    }
}
```

The CLI previews a prune and removes the confirmed ids.

```rust
let preview = worktrees.prune_selection(Prune { older_than_ms: Some(age), keep: None, force }).await?;
for candidate in confirmed(&preview) {
    worktrees.remove(Remove { task_id: candidate.task_id, force }).await?;
}
```

## Own contract

### Dependencies

`store` for the directory records, the Session associations, the stored workspaces and the removal claim; `managementMethods` for the `worktree.*` client methods. Git is the owner's own process resource, started and reaped by this owner.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `create` | Allocate a task id and make a Git worktree and branch for a Session creation | `{source, base?, branch?}` | `{taskId, repository, root, subtree, branch, base}` | Between Git calls and before the branch is created; a cancelled creation removes its empty task directory | `unavailable`, `invalid`, `branch_exists`, `git`, `interrupted` |
| `discard` | Settle a created worktree that the Session creation did not adopt | `{taskId}` | `{}` | Not cancellable: the cleanup runs to its end | `unknown_preparation`, `in_use`, `git` |
| `list` | Managed worktrees with Git state and Session use | `{}` | `{worktrees}` | While the views are built | `unavailable`, `storage` |
| `pruneSelection` | Candidates a prune would delete and the kept ones with reasons | `{olderThanMs? , keep?, force}` | `{candidates, kept}` | While the selection is built | `invalid`, `unavailable`, `storage` |
| `remove` | Remove one managed worktree, or an unregistered directory, with its record | `{taskId, force}` | `{removed}` | Before the Git removal; after it starts the removal completes or reports `git` | `invalid`, `not_found`, `in_use`, `git`, `storage` |

`create` returns a value; `discard` and `remove` take the task id. No object crosses. The preparation table is the owner's generation state: a reload drops it, and `discard` for a task id the generation did not create answers `unknown_preparation`; the directory is then an unregistered directory that `remove` handles.

`remove` composes the store: `claimWorktree` (the record, the associations and the stored workspaces), the Git removal, then `settleWorktree` (delete the record on success, restore it on failure). The claim id is bound to this owner's calling row, so an unload restores the record. `force` overrides only the content check; the Git removal receives `--force` only then.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `unavailable` | The daemon has no managed root or the source is not a usable Git worktree; the caller refuses the worktree request |
| `invalid` | Fix the request: an unknown task id form, both or neither of older-than and keep, a path that is not a plain managed directory |
| `branch_exists` | The requested branch already exists; choose another branch |
| `not_found` | No recorded or unregistered directory has the task id |
| `in_use` | A Session still uses the directory, or its content is not removable without `force`; the caller reports the blocker |
| `git` | Git refused the operation; the caller reports the message and keeps the directory and the report |
| `interrupted` | The call was stopped; a creation's partial state is cleaned, a removal is retried |
| `unknown_preparation` | The preparation ended with the generation; the directory is unregistered and `remove` handles it |
| `storage` | A record read or write failed; the caller reports the failure and keeps the record |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create` | Resolve the source and the Git roots, resolve the base, allocate a task id, `git worktree add -b`, canonicalize the root, check the subtree, return the value; the caller adopts it with `store.create` | Each Git call ends `interrupted`; an empty task directory this call made is removed; a created worktree is rolled back by the owner | The call ends `interrupted`; the rollback path runs | New calls are refused; an in-flight creation is rolled back or left as an unregistered directory | Git and the directory by ManagedWorktrees; the record by the store |
| `discard` | Look up the preparation, `claimWorktree`, Git removal, delete the branch, `settleWorktree` | No cancellation; the cleanup is the owner's | The preparation ends with the row; the directory is unregistered | The table drops with the generation; the directory stays unregistered | Directory and branch by ManagedWorktrees; record by the store |
| `list` | Read the records and associations, join Git state per directory, add unregistered directories | Before the reply | Nothing to release | Pending calls end `closed` | ManagedWorktrees |
| `pruneSelection` | `list`, then apply age, keep and force rules per candidate | Before the reply | Nothing to release | Pending calls end `closed` | ManagedWorktrees |
| `remove` | Resolve the record or the unregistered directory, `claimWorktree`, Git removal, `settleWorktree` | Before the claim or the Git removal; a started removal completes or reports `git` | The claim ends with the row; the record is restored | The claim drops with the store generation; the record stays for a retry | Directory by ManagedWorktrees; record by the store |

A process death between `create` and `store.create` leaves an unregistered directory; `list` reports it and `remove` deletes it after the operator names the task id. A process death between the claim and the settle leaves the record; the next `list` reports the directory as missing or no longer registered, and `remove` cleans the record.

## Exceptions with reasons

- A branch is kept when `remove` deletes a worktree: committed content must not disappear with the directory.
- `discard` is not cancellable: a cancelled Session creation still has to settle what the owner made.
- Unregistered directories are never pruned: the owner has no record to age, so only an explicit `remove` by task id deletes them.
- Git is the owner's process resource: the owner starts each call, bounds it (10s read, 30s write) and reaps it on cancellation.

## Verification

Flow tests on the owner:

- `create` makes a worktree and branch under the root; a taken task id or branch retries with a new task id; an explicit existing branch answers `branch_exists`; a cancelled creation leaves no created worktree and no empty task directory.
- `discard` removes a created worktree that no Session adopted; a Session association or a stored workspace keeps it and answers `in_use`; `unknown_preparation` after a generation change.
- `remove` deletes a recorded directory and its record; a directory with content refuses without `force`; a missing root deletes the record only; an unregistered directory with content refuses without `force`; a link is refused.
- `pruneSelection` never selects an unregistered, unknown, used or unremovable directory; `keep` treats an unknown use time as oldest; `olderThanMs` never selects an unknown time.
- A claim is restored when the calling row unloads; a settle after the claim ended answers `unknown_claim`.

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
| §7.2 ids and values | Task ids, paths and policies cross as data; the created value is data; no object crosses. |
| §7.2 one public face | The service is marked on the ManagedWorktrees type under the existing `managedWorktrees` name; the client methods `worktree.*` are registration points, not a second face. |
| §7.2 caller mechanisms | The root, Git, the preparation table and the claim stay inside the owner; callers handle the business errors only. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin; the `worktree.*` rows register methods, they do not own state. |
| ADR 002 D3 | Depends on the store for records; no lower handle. |
| ADR 002 D4 | The root is structural configuration; no business setting. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None new. The store's `claimWorktree` and `settleWorktree` are defined in [SessionStore's contract](SessionStore.md); the `managementMethods` registration point is the existing mechanism.

## Binding appendix

The service is marked on the owner type; the wire name stays `managedWorktrees`. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub task_id: String,
    pub repository: String,
    pub root: String,
    pub subtree: String,
    pub branch: String,
    pub base: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discard {
    pub task_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remove {
    pub task_id: String,
    pub force: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    BranchExists(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    InUse(String),
    #[error("{0}")]
    Git(String),
    #[error("{0}")]
    Interrupted(String),
    #[error("{0}")]
    UnknownPreparation(String),
    #[error("{0}")]
    Storage(String),
}

impl PublicService for ManagedWorktrees {}

#[rutis::service(name = "managedWorktrees")]
impl ManagedWorktrees {
    pub async fn create(
        &self,
        caller: &Ctx,
        request: Create,
        cancelled: CancelledFn,
    ) -> Result<Created, WorktreeError> {
        // ...
    }

    pub async fn discard(
        &self,
        caller: &Ctx,
        request: Discard,
        cancelled: CancelledFn,
    ) -> Result<(), WorktreeError> {
        // ...
    }

    pub async fn remove(
        &self,
        caller: &Ctx,
        request: Remove,
        cancelled: CancelledFn,
    ) -> Result<Removed, WorktreeError> {
        // ...
    }
}
```

`list` and `pruneSelection` follow the same shape, with the views and the prune selection as reply data.
