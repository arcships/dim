# File access facts

Scope: session. Issue: #2025.

The owner answers what one call's target path is and which file-access facts hold for it: inside the workspace, inside a system path, an owned Session file, or covered by a grant. Permission asks before approving a file call; the process preparation asks for the confirmed workspace roots before running a command. Today the same need is the `FileAccessFacts` object and its `ReadFileAccessFacts` trait; this document replaces both with two reads.

## Use and scope: responsibility split

The owner answers:

- **The facts.** Workspace, system, owned and grant membership under the branch's effective access preset and the grant mode the call asks for, plus the reason a workspace discovery failed when it did.
- **The confirmed roots.** The workspace and the managed worktree roots a command's process boundary may use.

Callers keep:

- What the facts mean: Permission's risk and approval decision, and which facts it shows.
- Which paths a tool is asked to read or write.
- The process boundary decision: the command preparation applies the roots it is given.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Permission decides a file call with the facts under the branch's access preset | No: the grant rule reads the preset, and the owned and grant facts come from two owners the caller would otherwise each hold | `check`; Permission keeps resolving the target, as ADR 014 D3 assigns it |
| A command's preparation needs the confirmed workspace boundary | No: the workspace and the managed worktree roots are validated against the Session's workspace; a caller that composed them could start a process outside the boundary | `workspaceRoots` |

## Caller example

```rust
// Permission: one file call's target and facts under the requested access.
let facts = access
    .check(Check { access: FileAccess::WorkspaceReadWrite, mode: GrantMode::Write, path: resolved }, cancelled)
    .await?;
let inside = facts.workspace || facts.system || facts.owned || facts.grant;
```

```rust
// HostPlatform: the boundary a command may use.
let roots = access.workspace_roots(WorkspaceRoots { cwd }, cancelled).await?;
```

## Own contract

### Dependencies

- The Session's working directory and identity: the owner's own Session part.
- `P:fileStorage` — `stat` and the path validation the workspace root uses.
- `P:sessionFiles` — whether the Session owns the target file.
- `S:sessionGrants` — the Session's file grants, read by `check` only.
- `P:worktrees` — managed worktree roots that extend the workspace.
- `P:hostPlatform` — the platform roots the `system` fact compares against.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `check` | Answer the facts of one resolved path under the branch's access preset | `{access, mode, path}` | `{workspace, system, owned, grant, discoveryError?}` | The caller drops the call; the reads stop | `invalid`, `unavailable`, `cancelled`, `closed` |
| `workspaceRoots` | The workspace and managed worktree roots a command may use | `{cwd}` | `{roots}` | The caller drops the call | `invalid`, `unavailable`, `cancelled`, `closed` |

`access` is the branch's access preset (`dim_agent_types::FileAccess`: `read-only`, `workspace-read-write`, `unrestricted`); `mode` is the grant mode the call asks for (`dim_agent_types::grant::GrantMode`: `read`, `write`); `path` is the path the caller already resolved, as ADR 014 D3 assigns resolution to Permission. A write grant is inert under `read-only` and active under `workspace-read-write`; `unrestricted` does not widen grants because it already allows every write.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `invalid` | both | Fix the input: an empty path, or a path the workspace cannot resolve |
| `unavailable` | both | The owned-file or grant facts could not be read; Permission denies the call, and a command is not started with an unconfirmed boundary |
| `cancelled` | both | Nothing was decided; the caller's own cancellation ended the read |
| `closed` | both | The Session is closing; the call is refused |

`discoveryError` is a field, not an error: a failed workspace discovery answers `workspace: false` with the reason, and the caller keeps the original file approvals visible with that reason.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `check` | The owner answers the facts for the caller's resolved path, reading the owned file and the grants | A dropped call ends the reads; nothing is decided | An in-flight call ends `closed` | The calling row; the owner holds no resource |
| `workspaceRoots` | The owner resolves the cwd, returns the workspace and the managed worktree roots | A dropped call ends the read | `closed` | The calling row |

## Exceptions with reasons

- No `sessionId` in the request: the owner is the Session's instance, so the Session id is the owner's own identity; a request field would let one Session ask for another's facts.
- `discoveryError` is a field: a workspace that cannot be discovered is a fact the caller shows, not a refusal; approvals the user already gave stay available.
- `access` is the three-valued access preset, not a read/write flag: under `read-only` a write grant is inert, and the owner decides that from the preset the caller passes.
- `workspaceRoots` fails rather than answering an empty list: a command started with an unconfirmed boundary would run outside the workspace the user approved.

## Verification

- `check` answers the four facts for the resolved path; a write grant under `read-only` answers `grant: false`; a relative path is `invalid`.
- An owned Session file and a granted path each answer their fact under the requested mode; a path outside every fact answers all false.
- A failed discovery answers `workspace: false` with `discoveryError` and no error.
- `workspaceRoots` answers the workspace plus the managed worktree roots; `unavailable` when the workspace cannot be read.
- A call in flight when the Session closes ends `closed`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part publishing two reads; it writes no Timeline event and folds no state. |
| §2 scopes | Session: the facts derive from the Session's workspace, files and grants. |
| §3 orthogonality | Reads the Session's own owners and process services; changes nothing. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handover of a business object; the facts are data. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning: Permission's risk rules stay with Permission. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path. |
| §4 rule 7 | Callers use the reads; no caller holds the owner's path state. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and objects | Every reply is a value; the old `Arc<dyn ReadFileAccessFacts>` no longer crosses. |
| §7 one interface | One name, `fileAccessFacts`, on the owner type. |
| §7 cross-language | Requests and replies are data; the path is a UTF-8 string. |
| ADR 002 D1 | Session: the facts are the Session's workspace and grants. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Reads process services at an enclosing scope and Session owners at its own. |
| ADR 002 D5 | Data contract; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Supersedes

This contract replaces the object shape of the file-access facts; the delivery in #2025 carries the migration.

- [ADR 014](../../adr/014-file-access-facts.md): the query is stated here as a Session-scoped operation `check`, with the Session instance supplying the session id and `FileAccess` and `GrantMode` as the same query arguments the ADR names. Resolution stays with Permission, as ADR 014 D3 assigns it; the reply carries the four facts and no `target`.
- `crate::file_access::ReadFileAccessFacts` and the `FileAccessFacts` wrapper: the two reads replace the trait and the object.

Rule D: no client wire protocol or user-visible behavior changes; the change is plugin-facing.

## Binding appendix

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::grant::GrantMode;
use dim_agent_types::FileAccess;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    pub access: FileAccess,
    pub mode: GrantMode,
    pub path: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFactsReply {
    pub workspace: bool,
    pub system: bool,
    pub owned: bool,
    pub grant: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRoots {
    pub cwd: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Roots {
    pub roots: Vec<String>,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FactsError {
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    Unavailable { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    Closed { message: String },
}

impl PublicService for FileAccessFacts {}

#[rutis::service(name = "fileAccessFacts")]
impl FileAccessFacts {
    pub async fn check(&self, request: Check, cancelled: CancelledFn) -> Result<FileFactsReply, FactsError> { /* ... */ }
    pub async fn workspace_roots(
        &self,
        request: WorkspaceRoots,
        cancelled: CancelledFn,
    ) -> Result<Roots, FactsError> { /* ... */ }
}
```

`cancelled` is the binding's cancellation argument, not a request field. Paths cross as UTF-8 strings; a path that is not UTF-8 is `invalid`. Errors cross as `{code, message}`, `code` from the variant name.
