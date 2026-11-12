//! Managed worktrees under the product home (#1451).
//!
//! One Process part owns the root, the task id, the Git calls and the
//! list/remove/prune selection. The records stay with SessionStore: creation
//! writes them in the Session's own transaction, and a removal runs under the
//! store's ownership lock so a late association cannot slip in between the
//! re-check and the record deletion. Git is the fact; a record is a candidate.
use crate::methods::ManagementMethods;
use crate::storage::{PreparedWorktree, SessionStore, WorktreeDirectory, WorktreeDirectoryRecord};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use dim_agent_runtime::{AgentError, Names};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long one Git call may take before the operation fails and the child is
/// killed. Worktree creation writes many files; the reads are small.
const GIT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded retries when a task id or its default branch is taken.
const ALLOCATION_ATTEMPTS: usize = 3;

/// The managed worktree root and the operations over it. It owns no Session
/// or branch state; the records belong to SessionStore and Git to the host.
pub struct ManagedWorktrees {
    root: PathBuf,
    sessions: Arc<SessionStore>,
}

/// A worktree this call created, with what it needs to be reported or
/// discarded: the directory, the Session's subtree, the branch and the base.
#[derive(Debug, Clone)]
pub struct CreatedWorktree {
    pub task_id: String,
    pub prepared: PreparedWorktree,
    pub branch: String,
    pub base: String,
}

/// One row of `worktree.list`: a recorded directory under the managed root,
/// or an unregistered directory a failed creation left behind.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeView {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub dirty: usize,
    pub untracked: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<i64>,
    pub sessions: Vec<String>,
    pub missing: bool,
    /// The directory exists but Git no longer names it as a worktree.
    pub git_missing: bool,
    pub unregistered: bool,
}

/// The selection a prune would delete and what it keeps, each with a reason.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneSelection {
    pub candidates: Vec<WorktreeView>,
    pub kept: Vec<KeptWorktree>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptWorktree {
    pub worktree: WorktreeView,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub enum WorktreeError {
    /// The daemon has no product home, or the path is not a Git workspace.
    Unavailable(String),
    Invalid(String),
    BranchExists(String),
    NotFound(String),
    InUse(String),
    Git(String),
    Store(String),
    /// The caller's connection closed or its load unloaded.
    Interrupted(String),
}

impl WorktreeError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "worktree_unavailable",
            Self::Invalid(_) => "worktree_invalid",
            Self::BranchExists(_) => "worktree_branch_exists",
            Self::NotFound(_) => "worktree_not_found",
            Self::InUse(_) => "worktree_in_use",
            Self::Git(_) => "worktree_git_failed",
            Self::Store(_) => "worktree_store_failed",
            Self::Interrupted(_) => "worktree_interrupted",
        }
    }
    fn message(&self) -> &str {
        match self {
            Self::Unavailable(message)
            | Self::Invalid(message)
            | Self::BranchExists(message)
            | Self::NotFound(message)
            | Self::InUse(message)
            | Self::Git(message)
            | Self::Store(message)
            | Self::Interrupted(message) => message,
        }
    }
    pub fn rpc(&self) -> RpcError {
        RpcError::app(-32000, self.code(), self.message())
    }
}

impl From<AgentError> for WorktreeError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::InvalidInput(message) => Self::InUse(message),
            other => Self::Store(other.to_string()),
        }
    }
}

impl ManagedWorktrees {
    pub fn new(root: PathBuf, sessions: Arc<SessionStore>) -> Self {
        Self { root, sessions }
    }

    /// Creates one worktree for `source` at `$HOME/worktrees/<task-id>/<repo>`
    /// and returns the directory and the subtree the new Session starts in.
    /// The caller records them in the Session creation transaction and calls
    /// [`Self::discard`] when that creation fails afterwards.
    pub async fn create(
        &self,
        source: &Path,
        base: Option<&str>,
        branch: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<CreatedWorktree, WorktreeError> {
        let source = existing_directory(source).await?;
        let bare = self
            .git(&source, &["rev-parse", "--is-bare-repository"], cancel)
            .await?;
        if bare.trim() == "true" {
            return Err(WorktreeError::Unavailable(
                "a bare repository has no worktree to start from".into(),
            ));
        }
        let top = PathBuf::from(
            self.git(&source, &["rev-parse", "--show-toplevel"], cancel)
                .await?
                .trim(),
        );
        let top = canonical(top).await?;
        let source = canonical(source).await?;
        if !source.starts_with(&top) {
            return Err(WorktreeError::Unavailable(
                "the workspace is outside its Git worktree".into(),
            ));
        }
        let repository = PathBuf::from(
            self.git(
                &source,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                cancel,
            )
            .await?
            .trim(),
        );
        let repository = canonical(repository).await?;
        let base = base.unwrap_or("HEAD");
        let base = self
            .git(&top, &["rev-parse", "--verify", base], cancel)
            .await?
            .trim()
            .to_owned();
        let relative = source
            .strip_prefix(&top)
            .map_err(|_| WorktreeError::Unavailable("the workspace left its Git root".into()))?
            .to_path_buf();
        let repo_name = top
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| WorktreeError::Unavailable("the repository has no name".into()))?
            .to_owned();
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|error| WorktreeError::Store(error.to_string()))?;
        let root = canonical(self.root.clone()).await?;

        let mut last = None;
        for _ in 0..ALLOCATION_ATTEMPTS {
            let task_id = task_id();
            let directory = root.join(&task_id).join(&repo_name);
            if tokio::fs::try_exists(&directory)
                .await
                .map_err(|error| WorktreeError::Store(error.to_string()))?
            {
                last = Some(WorktreeError::Invalid(
                    "the task id directory is already taken".into(),
                ));
                continue;
            }
            let branch_name = match branch {
                Some(branch) => branch.to_owned(),
                None => format!("dim/{task_id}"),
            };
            if self.branch_exists(&top, &branch_name, cancel).await? {
                if branch.is_some() {
                    return Err(WorktreeError::BranchExists(branch_name));
                }
                last = Some(WorktreeError::BranchExists(branch_name));
                continue;
            }
            let Some(parent) = directory.parent() else {
                return Err(WorktreeError::Invalid("the worktree has no parent".into()));
            };
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| WorktreeError::Store(error.to_string()))?;
            let directory_text = directory
                .to_str()
                .ok_or_else(|| WorktreeError::Invalid("the worktree path is not UTF-8".into()))?
                .to_owned();
            let added = git(
                &top,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch_name,
                    &directory_text,
                    &base,
                ],
                cancel,
                GIT_WRITE_TIMEOUT,
            )
            .await;
            if let Err(error) = added {
                // The path and branch were free when this call checked, so a
                // failure may belong to another actor that won the race: never
                // remove either. Only the empty task directory this call
                // created is ours to drop.
                last = Some(error);
                if let Some(parent) = directory.parent() {
                    let _ = tokio::fs::remove_dir(parent).await;
                }
                continue;
            }
            let root_dir = match canonical(directory).await {
                Ok(root_dir) => root_dir,
                Err(error) => {
                    let cleanup = CancellationToken::new();
                    let _ = self
                        .git(
                            &top,
                            &["worktree", "remove", "--force", directory_text.as_str()],
                            &cleanup,
                        )
                        .await;
                    let _ = self
                        .git(&top, &["branch", "-D", &branch_name], &cleanup)
                        .await;
                    return Err(error);
                }
            };
            let subtree = match checked_subtree(&root_dir, &relative).await {
                Ok(subtree) => subtree,
                Err(error) => {
                    let cleanup = CancellationToken::new();
                    let _ = self
                        .git(
                            &top,
                            &["worktree", "remove", "--force", directory_text.as_str()],
                            &cleanup,
                        )
                        .await;
                    let _ = self
                        .git(&top, &["branch", "-D", &branch_name], &cleanup)
                        .await;
                    return Err(error);
                }
            };
            return Ok(CreatedWorktree {
                task_id,
                prepared: PreparedWorktree {
                    directory: WorktreeDirectory {
                        repository,
                        root: root_dir,
                    },
                    subtree,
                },
                branch: branch_name,
                base,
            });
        }
        Err(last.unwrap_or_else(|| WorktreeError::Invalid("no task id was free".into())))
    }

    /// Removes what a failed creation made: the worktree and the branch this
    /// call created. A failure leaves both for the operator, reported by the
    /// caller; a retry never reuses the task id.
    pub async fn discard(&self, created: &CreatedWorktree, _cancel: &CancellationToken) {
        let root = &created.prepared.directory.root;
        let source = &created.prepared.directory.repository;
        let Some(root_text) = root.to_str() else {
            return;
        };
        // Cleanup must run after the caller's connection gave up; a cancelled
        // token would skip both Git calls and leak the registration. No
        // `--force`: content that appeared since the creation stays.
        let cancel = CancellationToken::new();
        let removed = self
            .git(source, &["worktree", "remove", root_text], &cancel)
            .await
            .is_ok();
        if !removed {
            return;
        }
        let _ = tokio::fs::remove_dir_all(root).await;
        if let Some(parent) = root.parent() {
            let _ = tokio::fs::remove_dir(parent).await;
        }
        let _ = self
            .git(source, &["branch", "-D", &created.branch], &cancel)
            .await;
    }

    /// Lists the recorded directories under the managed root, joined with Git
    /// and with the Session associations. Unregistered directories are listed
    /// with their task id; they are never removed by `prune`.
    pub async fn list(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<WorktreeView>, WorktreeError> {
        let root = match canonical(self.root.clone()).await {
            Ok(root) => root,
            Err(WorktreeError::Store(_))
                if !tokio::fs::try_exists(&self.root).await.unwrap_or(false) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let records = self
            .sessions
            .worktree_directories()
            .await
            .map_err(WorktreeError::from)?;
        let mut views = Vec::new();
        let mut recorded = Vec::new();
        for record in records {
            let WorktreeDirectoryRecord {
                directory,
                last_used_at_ms,
            } = record;
            if !directory.root.starts_with(&root) {
                continue;
            }
            recorded.push(directory.root.clone());
            let Some(task_id) = directory
                .root
                .parent()
                .and_then(Path::file_name)
                .and_then(OsStr::to_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let sessions = self
                .sessions
                .worktree_usages(directory.clone())
                .await
                .map_err(WorktreeError::from)?
                .into_iter()
                .map(|usage| usage.session_id)
                .collect();
            views.push(
                self.view(
                    &task_id,
                    directory.root.clone(),
                    Some(directory.repository.clone()),
                    last_used_at_ms,
                    sessions,
                    cancel,
                )
                .await,
            );
        }
        let mut entries = match tokio::fs::read_dir(&root).await {
            Ok(mut entries) => {
                let mut names = Vec::new();
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|error| WorktreeError::Store(error.to_string()))?
                {
                    names.push(entry.path());
                }
                names
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(WorktreeError::Store(error.to_string())),
        };
        entries.sort();
        for entry in entries {
            let Some(task_id) = entry.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if !valid_task_id(task_id) {
                continue;
            }
            // A task directory is ours; a symlink there is not.
            if !tokio::fs::symlink_metadata(&entry)
                .await
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            {
                continue;
            }
            let Ok(mut children) = tokio::fs::read_dir(&entry).await else {
                continue;
            };
            while let Some(child) = children
                .next_entry()
                .await
                .map_err(|error| WorktreeError::Store(error.to_string()))?
            {
                if recorded.contains(&child.path()) {
                    continue;
                }
                if !tokio::fs::symlink_metadata(child.path())
                    .await
                    .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                {
                    continue;
                }
                views.push(
                    self.view(task_id, child.path(), None, None, Vec::new(), cancel)
                        .await,
                );
            }
        }
        views.sort_by(|left, right| {
            left.task_id
                .cmp(&right.task_id)
                .then(left.root.cmp(&right.root))
        });
        Ok(views)
    }

    async fn view(
        &self,
        task_id: &str,
        root: PathBuf,
        repository: Option<PathBuf>,
        last_used_at_ms: Option<i64>,
        sessions: Vec<String>,
        cancel: &CancellationToken,
    ) -> WorktreeView {
        let mut view = WorktreeView {
            task_id: task_id.to_owned(),
            repository: repository
                .as_ref()
                .map(|repository| repository.to_string_lossy().into_owned()),
            root: root.to_string_lossy().into_owned(),
            branch: None,
            head: None,
            dirty: 0,
            untracked: 0,
            last_used_at_ms,
            sessions,
            missing: !tokio::fs::try_exists(&root).await.unwrap_or(false),
            git_missing: false,
            unregistered: repository.is_none(),
        };
        if view.missing || view.unregistered {
            return view;
        }
        let Ok(branch) = self
            .git(&root, &["rev-parse", "--abbrev-ref", "HEAD"], cancel)
            .await
        else {
            view.git_missing = true;
            return view;
        };
        let branch = branch.trim().to_owned();
        view.branch = (branch != "HEAD").then_some(branch);
        if let Ok(head) = self
            .git(&root, &["rev-parse", "--short", "HEAD"], cancel)
            .await
        {
            view.head = Some(head.trim().to_owned());
        }
        if let Ok(status) = self
            .git(&root, &["status", "--porcelain", "--ignored"], cancel)
            .await
        {
            for line in status.lines().filter(|line| !line.is_empty()) {
                if line.starts_with("??") || line.starts_with("!!") {
                    view.untracked += 1;
                } else {
                    view.dirty += 1;
                }
            }
        }
        view
    }

    /// Selects what `prune` would delete: managed, recorded directories with
    /// no remaining association, aged by the directory record's last use.
    /// Unknown times never satisfy `--older-than` and count as oldest for
    /// `--keep`; unregistered and external directories are never selected.
    pub async fn prune_selection(
        &self,
        older_than: Option<Duration>,
        keep: Option<usize>,
        force: bool,
        cancel: &CancellationToken,
    ) -> Result<PruneSelection, WorktreeError> {
        let all = self.list(cancel).await?;
        let mut candidates = Vec::new();
        let mut kept = Vec::new();
        for view in all {
            if view.unregistered {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: "unregistered; remove it by task id".into(),
                });
                continue;
            }
            if !view.sessions.is_empty() {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: "a Session still has an association".into(),
                });
                continue;
            }
            if view.git_missing && !force {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: "the directory is no longer a Git worktree; --force removes it".into(),
                });
                continue;
            }
            let users = self
                .sessions
                .sessions_under_workspace(Path::new(&view.root))
                .await
                .map_err(WorktreeError::from)?;
            if let Some(session) = users.first() {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: format!("Session {session} has its workspace at or under it"),
                });
                continue;
            }
            if !force && (view.dirty > 0 || view.untracked > 0) {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: "uncommitted or untracked content; --force removes it".into(),
                });
                continue;
            }
            if let Some(older_than) = older_than {
                let Some(time) = view.last_used_at_ms else {
                    kept.push(KeptWorktree {
                        worktree: view,
                        reason: "no recorded last use; --older-than cannot select it".into(),
                    });
                    continue;
                };
                let now = dim_agent_runtime::core::unix_millis() as i64;
                if now.saturating_sub(time) < older_than.as_millis() as i64 {
                    kept.push(KeptWorktree {
                        worktree: view,
                        reason: "used more recently than --older-than".into(),
                    });
                    continue;
                }
            }
            candidates.push(view);
        }
        if let Some(keep) = keep {
            candidates.sort_by_key(|view| view.last_used_at_ms.unwrap_or(i64::MIN));
            let keep_from = candidates.len().saturating_sub(keep);
            for view in candidates.drain(keep_from..) {
                kept.push(KeptWorktree {
                    worktree: view,
                    reason: format!("kept by --keep {keep}"),
                });
            }
        }
        Ok(PruneSelection { candidates, kept })
    }

    /// Deletes one managed worktree by task id, or an unregistered directory
    /// that Git does not know. `force` overrides only the content check.
    pub async fn remove(
        &self,
        task_id: &str,
        force: bool,
        cancel: &CancellationToken,
    ) -> Result<bool, WorktreeError> {
        if !valid_task_id(task_id) {
            return Err(WorktreeError::Invalid(
                "a task id is six lowercase base32 characters".into(),
            ));
        }
        let root = match canonical(self.root.clone()).await {
            Ok(root) => root,
            Err(WorktreeError::Store(_))
                if !tokio::fs::try_exists(&self.root).await.unwrap_or(false) =>
            {
                return Err(WorktreeError::NotFound(format!(
                    "no managed worktree has task id {task_id}"
                )));
            }
            Err(error) => return Err(error),
        };
        let records = self
            .sessions
            .worktree_directories()
            .await
            .map_err(WorktreeError::from)?;
        let record = records.into_iter().find(|record| {
            record.directory.root.starts_with(&root)
                && record
                    .directory
                    .root
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(OsStr::to_str)
                    == Some(task_id)
        });
        let Some(record) = record else {
            return self
                .remove_unregistered(task_id, &root, force, cancel)
                .await;
        };
        let directory = record.directory;
        let repository = directory.repository.clone();
        let sessions = self.sessions.clone();
        let cancel = cancel.clone();
        let removed = sessions
            .remove_worktree(directory, move |checked| {
                Box::pin(async move {
                    if !tokio::fs::try_exists(&checked).await.unwrap_or(false) {
                        let _ = git(
                            &repository,
                            &["worktree", "prune"],
                            &cancel,
                            GIT_READ_TIMEOUT,
                        )
                        .await;
                        return Ok(());
                    }
                    let registered = git(
                        &checked,
                        &["rev-parse", "--show-toplevel"],
                        &cancel,
                        GIT_READ_TIMEOUT,
                    )
                    .await
                    .is_ok();
                    if !registered {
                        return cleanup_unregistered_root(&repository, &checked, force, &cancel)
                            .await;
                    }
                    if !force {
                        let status = match git(
                            &checked,
                            &["status", "--porcelain", "--ignored"],
                            &cancel,
                            GIT_READ_TIMEOUT,
                        )
                        .await
                        {
                            Ok(status) => status,
                            Err(error) => {
                                return Err(AgentError::InvalidInput(error.message().to_owned()));
                            }
                        };
                        if !status.trim().is_empty() {
                            return Err(AgentError::InvalidInput(
                                "the worktree has uncommitted or untracked content".into(),
                            ));
                        }
                    }
                    let root_text = checked.to_string_lossy();
                    let mut args = vec!["worktree", "remove"];
                    if force {
                        args.push("--force");
                    }
                    args.push(root_text.as_ref());
                    git(&repository, &args, &cancel, GIT_WRITE_TIMEOUT)
                        .await
                        .map_err(|error| AgentError::Storage(error.message().to_owned()))?;
                    Ok(())
                })
            })
            .await
            .map_err(WorktreeError::from)?;
        Ok(removed)
    }

    async fn remove_unregistered(
        &self,
        task_id: &str,
        root: &Path,
        force: bool,
        _cancel: &CancellationToken,
    ) -> Result<bool, WorktreeError> {
        let entry = root.join(task_id);
        if !tokio::fs::try_exists(&entry)
            .await
            .map_err(|error| WorktreeError::Store(error.to_string()))?
        {
            return Err(WorktreeError::NotFound(format!(
                "no managed worktree has task id {task_id}"
            )));
        }
        if !tokio::fs::symlink_metadata(&entry)
            .await
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            return Err(WorktreeError::InUse(format!(
                "{} is not a plain directory under the managed root",
                entry.display()
            )));
        }
        let mut children = tokio::fs::read_dir(&entry)
            .await
            .map_err(|error| WorktreeError::Store(error.to_string()))?;
        while let Some(child) = children
            .next_entry()
            .await
            .map_err(|error| WorktreeError::Store(error.to_string()))?
        {
            let path = child.path();
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|error| WorktreeError::Store(error.to_string()))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let resolved = canonical(path.clone()).await?;
            if resolved != path || !resolved.starts_with(root) {
                return Err(WorktreeError::InUse(format!(
                    "{} resolves outside the managed root",
                    path.display()
                )));
            }
            if tokio::fs::try_exists(path.join(".git"))
                .await
                .unwrap_or(false)
            {
                return Err(WorktreeError::InUse(format!(
                    "{} is a Git worktree with no record; inspect it before deleting",
                    path.display()
                )));
            }
            let sessions = self
                .sessions
                .sessions_under_workspace(&path)
                .await
                .map_err(WorktreeError::from)?;
            if let Some(session) = sessions.first() {
                return Err(WorktreeError::InUse(format!(
                    "Session {session} uses {}",
                    path.display()
                )));
            }
            if !force {
                let mut empty = tokio::fs::read_dir(&path)
                    .await
                    .map_err(|error| WorktreeError::Store(error.to_string()))?;
                if empty
                    .next_entry()
                    .await
                    .map_err(|error| WorktreeError::Store(error.to_string()))?
                    .is_some()
                {
                    return Err(WorktreeError::InUse(format!(
                        "{} is not empty; --force removes it",
                        path.display()
                    )));
                }
            }
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|error| WorktreeError::Store(error.to_string()))?;
        }
        let _ = tokio::fs::remove_dir_all(&entry).await;
        Ok(true)
    }

    async fn branch_exists(
        &self,
        repository: &Path,
        branch: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, WorktreeError> {
        let reference = format!("refs/heads/{branch}");
        let status = git_status(
            repository,
            &["show-ref", "--verify", "--quiet", &reference],
            cancel,
        )
        .await?;
        Ok(status == 0)
    }

    async fn git(
        &self,
        cwd: &Path,
        args: &[&str],
        cancel: &CancellationToken,
    ) -> Result<String, WorktreeError> {
        git(cwd, args, cancel, GIT_READ_TIMEOUT).await
    }
}

async fn git(
    cwd: &Path,
    args: &[&str],
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<String, WorktreeError> {
    let output = git_output(cwd, args, cancel, timeout).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(WorktreeError::Git(if stderr.is_empty() {
            format!("Git {args:?} failed with {}", output.status)
        } else {
            stderr.to_owned()
        }));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| WorktreeError::Git("Git returned non-UTF-8 output".into()))
}

/// The exit code of one Git call, for the predicates that use it as the
/// answer (`show-ref --verify --quiet`).
async fn git_status(
    cwd: &Path,
    args: &[&str],
    cancel: &CancellationToken,
) -> Result<i32, WorktreeError> {
    let output = git_output(cwd, args, cancel, GIT_READ_TIMEOUT).await?;
    Ok(output.status.code().unwrap_or(-1))
}

async fn git_output(
    cwd: &Path,
    args: &[&str],
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<std::process::Output, WorktreeError> {
    if cancel.is_cancelled() {
        return Err(WorktreeError::Interrupted(
            "the operation was interrupted".into(),
        ));
    }
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(cwd).args(args).kill_on_drop(true);
    command.env("LC_ALL", "C");
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    let output = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return Err(WorktreeError::Interrupted("the operation was interrupted".into()));
        }
        output = tokio::time::timeout(timeout, command.output()) => output,
    };
    match output {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(WorktreeError::Git(format!(
            "Git could not be started: {error}"
        ))),
        Err(_) => Err(WorktreeError::Git(format!("Git {args:?} timed out"))),
    }
}

async fn canonical(path: PathBuf) -> Result<PathBuf, WorktreeError> {
    tokio::fs::canonicalize(&path)
        .await
        .map_err(|error| WorktreeError::Store(format!("{}: {error}", path.display())))
}

/// The Session's starting directory inside the new worktree: the mapped
/// source-relative prefix, with every component proven not to be a symlink.
/// A redirect would make a path outside the checkout (or a sibling subtree)
/// the Session workspace, which worktree-access §2.2 forbids.
async fn checked_subtree(root: &Path, relative: &Path) -> Result<PathBuf, WorktreeError> {
    let mut current = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err(WorktreeError::Invalid(
                "the source workspace does not map to a relative subtree".into(),
            ));
        };
        current.push(part);
        let metadata = tokio::fs::symlink_metadata(&current)
            .await
            .map_err(|error| WorktreeError::Store(format!("{}: {error}", current.display())))?;
        if metadata.file_type().is_symlink() {
            return Err(WorktreeError::Invalid(format!(
                "{} is a symlink; the Session subtree cannot be redirected",
                current.display()
            )));
        }
    }
    let subtree = canonical(current).await?;
    let root = canonical(root.to_path_buf()).await?;
    let expected = root.join(relative);
    if subtree != expected {
        return Err(WorktreeError::Invalid(
            "the Session subtree does not resolve inside the worktree root".into(),
        ));
    }
    Ok(subtree)
}

/// A recorded root that exists but Git no longer names: there is no working
/// tree to remove. An empty directory is deleted; other content needs the
/// caller's `--force`; the Git metadata is pruned either way.
async fn cleanup_unregistered_root(
    repository: &Path,
    root: &Path,
    force: bool,
    cancel: &CancellationToken,
) -> Result<(), AgentError> {
    let mut entries = tokio::fs::read_dir(root)
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))?;
    let empty = entries
        .next_entry()
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))?
        .is_none();
    if !empty && !force {
        return Err(AgentError::InvalidInput(format!(
            "{} is no longer a Git worktree and is not empty; --force removes it",
            root.display()
        )));
    }
    if !empty {
        tokio::fs::remove_dir_all(root)
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
    } else {
        tokio::fs::remove_dir(root)
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
    }
    let _ = git(repository, &["worktree", "prune"], cancel, GIT_READ_TIMEOUT).await;
    Ok(())
}

async fn existing_directory(path: &Path) -> Result<PathBuf, WorktreeError> {
    let mut path = path;
    loop {
        match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_dir() => return Ok(path.to_path_buf()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(WorktreeError::Store(error.to_string())),
        }
        path = path
            .parent()
            .ok_or_else(|| WorktreeError::Invalid("the workspace does not exist".into()))?;
    }
}

/// Six lowercase base32 characters from a fresh UUID. The id is assigned
/// here and is not the Session id: creation precedes the Session.
fn task_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let bits = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (0..6)
        .map(|index| ALPHABET[((bits >> (5 * (5 - index))) & 0x1f) as usize] as char)
        .collect()
}

fn valid_task_id(task_id: &str) -> bool {
    task_id.len() == 6
        && task_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
        && Path::new(task_id)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// The `session.create` reply for a managed worktree: the task id, the
/// management root, and the directory the Session starts in.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktreeReply {
    pub task_id: String,
    pub root: String,
    pub cwd: String,
    pub repository: String,
    pub base: String,
    pub branch: String,
}

impl CreatedWorktree {
    pub fn reply(&self) -> CreatedWorktreeReply {
        CreatedWorktreeReply {
            task_id: self.task_id.clone(),
            root: self.prepared.directory.root.to_string_lossy().into_owned(),
            cwd: self.prepared.subtree.to_string_lossy().into_owned(),
            repository: self
                .prepared
                .directory
                .repository
                .to_string_lossy()
                .into_owned(),
            base: self.base.clone(),
            branch: self.branch.clone(),
        }
    }
}

/// The `worktree` management namespace: host commands that never take part in
/// Permission approval and are never served to a paired device.
pub struct ManagedWorktreesPlugin {
    sessions: Key<SessionStore>,
    methods: Key<ManagementMethods>,
    root: PathBuf,
}

impl ManagedWorktreesPlugin {
    pub fn new(names: &Names<'_>, home: PathBuf) -> rutis::Result<Self> {
        Ok(Self {
            sessions: names.key()?,
            methods: names.key()?,
            root: home.join("worktrees"),
        })
    }
}

impl DynPlugin for ManagedWorktreesPlugin {
    fn name(&self) -> &str {
        "ManagedWorktrees"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.sessions.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let sessions = ctx.require_at(&self.sessions)?;
            let methods = ctx.require_at(&self.methods)?;
            let managed = Arc::new(ManagedWorktrees::new(self.root.clone(), sessions));
            ctx.provide_as::<ManagedWorktrees>(managed.clone())?;
            methods.register(
                ctx,
                "worktree",
                vec![
                    MethodDecl::new("worktree.list", false, Interruption::Report),
                    MethodDecl::new("worktree.remove", true, Interruption::Drain),
                    MethodDecl::new("worktree.prune", true, Interruption::Drain),
                ],
                Arc::new(move |call, cancel, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let managed = managed.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "worktree.list" => {
                                let _: ListParams = params(value)?;
                                let worktrees =
                                    managed.list(&cancel).await.map_err(|error| error.rpc())?;
                                reply(ListReply { worktrees })
                            }
                            "worktree.remove" => {
                                let request: RemoveParams = params(value)?;
                                let removed = managed
                                    .remove(&request.task_id, request.force, &cancel)
                                    .await
                                    .map_err(|error| error.rpc())?;
                                reply(RemoveReply { removed })
                            }
                            "worktree.prune" => {
                                let request: PruneParams = params(value)?;
                                if request.older_than.is_some() == request.keep.is_some() {
                                    return Err(RpcError::invalid(
                                        "exactly one of olderThan or keep is required",
                                    ));
                                }
                                let older_than = request
                                    .older_than
                                    .as_deref()
                                    .map(parse_duration)
                                    .transpose()?;
                                let selection = managed
                                    .prune_selection(
                                        older_than,
                                        request.keep,
                                        request.force,
                                        &cancel,
                                    )
                                    .await
                                    .map_err(|error| error.rpc())?;
                                let mut removed = Vec::new();
                                let mut failed = Vec::new();
                                if request.apply {
                                    let selected = if request.task_ids.is_empty() {
                                        selection
                                            .candidates
                                            .iter()
                                            .map(|candidate| candidate.task_id.clone())
                                            .collect()
                                    } else {
                                        request.task_ids.clone()
                                    };
                                    for task_id in selected {
                                        match managed.remove(&task_id, request.force, &cancel).await
                                        {
                                            Ok(_) => removed.push(task_id),
                                            Err(error) => {
                                                failed.push(PruneFailure {
                                                    task_id,
                                                    code: error.code().to_owned(),
                                                    message: error.message().to_owned(),
                                                });
                                            }
                                        }
                                    }
                                }
                                reply(PruneReply {
                                    candidates: selection.candidates,
                                    kept: selection.kept,
                                    removed,
                                    failed,
                                })
                            }
                            _ => unreachable!("registered worktree management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParams {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveParams {
    task_id: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PruneParams {
    #[serde(default)]
    older_than: Option<String>,
    #[serde(default)]
    keep: Option<usize>,
    /// The preview's task ids; an apply removes exactly these, rechecked.
    #[serde(default)]
    task_ids: Vec<String>,
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListReply {
    worktrees: Vec<WorktreeView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoveReply {
    removed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PruneReply {
    candidates: Vec<WorktreeView>,
    kept: Vec<KeptWorktree>,
    removed: Vec<String>,
    /// The candidates an apply could not remove; each keeps its record.
    failed: Vec<PruneFailure>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PruneFailure {
    task_id: String,
    code: String,
    message: String,
}

/// A count and a unit, `30d`, `12h` or `5m`, as `session cleanup` accepts.
fn parse_duration(text: &str) -> Result<Duration, RpcError> {
    let mut chars = text.chars();
    let unit = chars
        .next_back()
        .ok_or_else(|| RpcError::invalid(format!("{text} is not a duration")))?;
    let count: u64 = chars
        .as_str()
        .parse()
        .map_err(|_| RpcError::invalid(format!("{text} is not a duration")))?;
    let seconds = match unit {
        'd' => 24 * 60 * 60,
        'h' => 60 * 60,
        'm' => 60,
        _ => return Err(RpcError::invalid(format!("{text} is not a duration"))),
    };
    Ok(Duration::from_secs(count.checked_mul(seconds).ok_or_else(
        || RpcError::invalid(format!("{text} is too long")),
    )?))
}
