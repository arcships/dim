//! Session file facts and on-demand worktree membership.
use crate::{
    host::{HostError, HostFuture, SessionFilesHost},
    host_platform::{HostPlatform, ProcessSpec},
    session_grants::SessionGrantsHost,
    workspace_tools::files::canonical_prefix,
};
use dim_agent_runtime::{
    AgentBuilder, Feature, Names, SessionConfig, SessionIdentity, SessionLifecycle, SessionPhase,
};
use dim_agent_types::{FileAccess, grant::GrantMode};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, watch},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// A canonical Git common directory and worktree root. This records a
/// repository relationship, not permission, creation or deletion authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDirectory {
    pub repository: PathBuf,
    pub root: PathBuf,
}

/// One worktree association to record: the Session, the Git common directory
/// and worktree root it belongs to, and the Session's verified subtree. The
/// paths are UTF-8 strings, as every path that crosses is ([service faces
/// §1](../../../docs/design/service-faces.md#1-规则)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorktreeAssociation {
    pub session_id: String,
    pub repository: String,
    pub root: String,
    pub subtree: String,
}

impl WorktreeAssociation {
    /// The recorded directory, as the file-access owner passes it.
    pub fn directory(&self) -> WorktreeDirectory {
        WorktreeDirectory {
            repository: self.repository.clone().into(),
            root: self.root.clone().into(),
        }
    }
}

/// What `worktreeAssociations.associate` fails with: `{code, message}`.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WorktreeAssociationError {
    #[error("{message}")]
    Failed { message: String },
}

/// An owner's write of one association, bound as the face's one operation.
/// Missing Sessions and storage failures fail without publishing either
/// record; the closure says which by its message.
pub type WorktreeAssociationWrite = Arc<
    dyn Fn(WorktreeAssociation) -> BoxFuture<'static, Result<(), WorktreeAssociationError>>
        + Send
        + Sync,
>;

/// The `worktreeAssociations` face: the association store records a verified
/// directory and its Session subtree atomically. The owner binds its write; a
/// Rust reader calls [`Self::associate`] directly and a part of another
/// language calls the same operation by name.
pub struct WorktreeAssociations(WorktreeAssociationWrite);

impl WorktreeAssociations {
    /// Binds an owner's write as the face's operation.
    pub fn new(associate: WorktreeAssociationWrite) -> Self {
        Self(associate)
    }
}

#[rutis::service]
impl WorktreeAssociations {
    /// Records `request`'s directory and Session subtree as one association.
    pub async fn associate(
        &self,
        request: WorktreeAssociation,
    ) -> Result<(), WorktreeAssociationError> {
        (self.0)(request).await
    }
}

/// Facts for one resolved path under one branch's file-access policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    pub workspace: bool,
    pub system: bool,
    pub owned: bool,
    pub grant: bool,
    /// Discovery failed without expanding the workspace. Original file
    /// approvals remain available and include this reason.
    pub discovery_error: Option<String>,
}

/// The Session's resolved file facts and confirmed process workspace roots.
pub trait ReadFileAccessFacts: Send + Sync + 'static {
    fn facts(
        &self,
        session_id: &str,
        access: FileAccess,
        path: &Path,
        mode: GrantMode,
        cancel: CancellationToken,
    ) -> HostFuture<'_, FileFacts>;
    /// Recognizes the resolved cwd, then returns the original workspace and
    /// validated confirmed roots for process preparation.
    /// Failure or cancellation must not start a process with an expanded boundary.
    fn workspace_roots(
        &self,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Vec<PathBuf>>;
}
pub struct FileAccessFacts(pub Arc<dyn ReadFileAccessFacts>);

pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(
        Feature::new("FileAccessFacts").session("FileAccessFacts", |names| {
            Ok(Box::new(FileAccessFactsPlugin::new(names)?))
        }),
    );
}

struct Facts {
    config: Arc<SessionConfig>,
    platform: Arc<HostPlatform>,
    files: Arc<SessionFilesHost>,
    grants: Arc<SessionGrantsHost>,
    worktrees: Arc<Worktrees>,
}
impl ReadFileAccessFacts for Facts {
    fn facts(
        &self,
        session_id: &str,
        access: FileAccess,
        path: &Path,
        mode: GrantMode,
        cancel: CancellationToken,
    ) -> HostFuture<'_, FileFacts> {
        let path = path.to_path_buf();
        let session_id = session_id.to_owned();
        Box::pin(async move {
            let cwd = PathBuf::from(&self.config.cwd);
            let workspace = tokio::task::spawn_blocking(move || canonical_prefix(cwd))
                .await
                .map_err(|error| {
                    HostError::failed(format!("file access facts task failed: {error}"))
                })?
                .map_err(HostError::failed)?;
            let owned = self
                .files
                .0
                .owns(session_id, path.clone())
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let grants = self.grants.0.grants(cancel.clone()).await?;
            let mut facts = FileFacts {
                workspace: path.starts_with(&workspace),
                system: self
                    .platform
                    .platform_roots
                    .iter()
                    .any(|root| path.starts_with(root)),
                owned,
                grant: grants.iter().any(|grant| match mode {
                    GrantMode::Write => {
                        access == FileAccess::WorkspaceReadWrite && grant.matches(&path, mode)
                    }
                    GrantMode::Read => grant.matches(&path, mode),
                }),
                discovery_error: None,
            };
            if !facts.workspace
                && !facts.system
                && !facts.owned
                && !facts.grant
                && !(mode == GrantMode::Write && access != FileAccess::WorkspaceReadWrite)
            {
                match self.worktrees.contains(&path, cancel).await {
                    Ok(workspace) => facts.workspace = workspace,
                    Err(error @ HostError::Cancelled(_)) => return Err(error),
                    Err(error) => facts.discovery_error = Some(error.to_string()),
                }
            }
            Ok(facts)
        })
    }
    fn workspace_roots(
        &self,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Vec<PathBuf>> {
        let cwd = cwd.to_path_buf();
        Box::pin(async move { self.worktrees.roots(&cwd, cancel).await })
    }
}

struct FileAccessFactsPlugin {
    config: Key<SessionConfig>,
    platform: Key<HostPlatform>,
    files: Key<SessionFilesHost>,
    grants: Key<SessionGrantsHost>,
    identity: Key<SessionIdentity>,
    lifecycle: Key<SessionLifecycle>,
    service: Key<FileAccessFacts>,
}
impl FileAccessFactsPlugin {
    fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: names.key()?,
            platform: names.key()?,
            files: names.key()?,
            grants: names.key()?,
            identity: names.key()?,
            lifecycle: names.key()?,
            service: names.key()?,
        })
    }
}
impl DynPlugin for FileAccessFactsPlugin {
    fn name(&self) -> &str {
        "FileAccessFacts"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.config.erase(),
            self.platform.erase(),
            self.files.erase(),
            self.grants.erase(),
            self.identity.erase(),
            self.lifecycle.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            let platform = ctx.require_at(&self.platform)?;
            let worktrees = Worktrees::new(
                ctx.clone(),
                platform.clone(),
                ctx.require_at(&self.lifecycle)?,
                ctx.require_at(&self.identity)?.session_id.clone(),
                PathBuf::from(&config.cwd),
            );
            let owner = worktrees.clone();
            ctx.effect(move || async move {
                owner.close().await;
                Ok(())
            })?;
            ctx.provide_at(
                self.service.clone(),
                Arc::new(FileAccessFacts(Arc::new(Facts {
                    config,
                    platform,
                    files: ctx.require_at(&self.files)?,
                    grants: ctx.require_at(&self.grants)?,
                    worktrees,
                }))),
            )?;
            Ok(Effect::Done)
        })
    }
}

const METADATA_LIMIT: u64 = 64 * 1024;

#[derive(Clone)]
struct Root {
    directory: WorktreeDirectory,
    subtree: PathBuf,
    identity: Arc<File>,
}
impl Root {
    fn valid(&self) -> bool {
        let Ok(current) = open_directory(&self.directory.root) else {
            return false;
        };
        matches!((identity(&current), identity(&self.identity)), (Ok(a), Ok(b)) if a == b)
            && std::fs::canonicalize(&self.directory.root)
                .is_ok_and(|path| path == self.directory.root)
            && canonical_prefix(self.subtree.clone()).is_ok_and(|path| path == self.subtree)
    }
}

type ProbeResult = Result<Option<Root>, HostError>;
#[derive(Default)]
struct State {
    roots: Vec<Root>,
    probes: HashMap<PathBuf, watch::Receiver<Option<ProbeResult>>>,
}

struct Worktrees {
    ctx: Ctx,
    platform: Arc<HostPlatform>,
    lifecycle: Arc<SessionLifecycle>,
    session: String,
    workspace: PathBuf,
    source: Mutex<Option<(WorktreeDirectory, PathBuf)>>,
    state: Mutex<State>,
    stopping: CancellationToken,
    tasks: TaskTracker,
}
impl Worktrees {
    fn new(
        ctx: Ctx,
        platform: Arc<HostPlatform>,
        lifecycle: Arc<SessionLifecycle>,
        session: String,
        workspace: PathBuf,
    ) -> Arc<Self> {
        let stopping = ctx.cancellation_token().child_token();
        Arc::new(Self {
            ctx,
            platform,
            lifecycle,
            session,
            workspace,
            source: Mutex::new(None),
            state: Mutex::new(State::default()),
            stopping,
            tasks: TaskTracker::new(),
        })
    }

    async fn close(&self) {
        self.stopping.cancel();
        {
            let mut state = self.state.lock().await;
            self.tasks.close();
            state.roots.clear();
        }
        self.tasks.wait().await;
        *self.source.lock().await = None;
    }

    async fn contains(
        self: &Arc<Self>,
        path: &Path,
        cancel: CancellationToken,
    ) -> Result<bool, HostError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            _ = self.stopping.cancelled() => Err(cancelled()),
            _ = self.lifecycle.closing() => Err(cancelled()),
            result = self.confirm(path.to_path_buf()) => result,
        }
    }

    async fn confirm(self: &Arc<Self>, path: PathBuf) -> Result<bool, HostError> {
        if self.lifecycle.phase() != SessionPhase::Active {
            return Err(cancelled());
        }
        let mut state = self.state.lock().await;
        self.refresh(&mut state).await?;
        if state
            .roots
            .iter()
            .any(|root| path.starts_with(&root.subtree))
        {
            return Ok(true);
        }
        let associations = self
            .ctx
            .get::<WorktreeAssociations>()
            .ok_or_else(|| HostError::failed("Worktree association storage is unavailable"))?;
        let target = path.clone();
        let directory = blocking(move || existing_directory(&target)).await?;
        let mut receiver = if let Some(receiver) = state.probes.get(&directory) {
            receiver.clone()
        } else {
            if self.stopping.is_cancelled() {
                return Err(cancelled());
            }
            let (sender, receiver) = watch::channel(None);
            state.probes.insert(directory.clone(), receiver.clone());
            let owner = self.clone();
            self.tasks.spawn(async move {
                let result = owner.discover(&directory).await;
                sender.send_replace(Some(result));
                owner.state.lock().await.probes.remove(&directory);
            });
            receiver
        };
        drop(state);
        let discovered = receiver
            .wait_for(|value| value.is_some())
            .await
            .map_err(|_| HostError::failed("Worktree discovery ended without a result"))?
            .as_ref()
            .expect("the probe published its result")
            .clone()?;
        let Some(root) = discovered else {
            return Ok(false);
        };
        if !path.starts_with(&root.subtree) {
            return Ok(false);
        }
        let mut state = self.state.lock().await;
        self.refresh(&mut state).await?;
        if state
            .roots
            .iter()
            .any(|root| path.starts_with(&root.subtree))
        {
            return Ok(true);
        }
        let check = root.clone();
        if !blocking(move || Ok(check.valid())).await? {
            return Ok(false);
        }
        if self.stopping.is_cancelled() || self.lifecycle.phase() != SessionPhase::Active {
            return Err(cancelled());
        }
        let request = association(&self.session, &root)?;
        associations
            .associate(request)
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        if self.stopping.is_cancelled() || self.lifecycle.phase() != SessionPhase::Active {
            return Err(cancelled());
        }
        let check = root.clone();
        if !blocking(move || Ok(check.valid())).await? {
            return Ok(false);
        }
        state.roots.push(root);
        Ok(true)
    }

    async fn refresh(&self, state: &mut State) -> Result<(), HostError> {
        let roots = state.roots.clone();
        state.roots = blocking(move || Ok(roots.into_iter().filter(Root::valid).collect())).await?;
        Ok(())
    }

    async fn roots(
        self: &Arc<Self>,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> Result<Vec<PathBuf>, HostError> {
        let cwd = cwd.to_path_buf();
        let workspace = self.workspace.clone();
        let (cwd, workspace) = blocking(move || {
            Ok((
                canonical_prefix(cwd).map_err(HostError::failed)?,
                canonical_prefix(workspace).map_err(HostError::failed)?,
            ))
        })
        .await?;
        if !cwd.starts_with(&workspace) {
            self.contains(&cwd, cancel.clone()).await?;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            _ = self.stopping.cancelled() => Err(cancelled()),
            _ = self.lifecycle.closing() => Err(cancelled()),
            result = async {
                let mut state = self.state.lock().await;
                self.refresh(&mut state).await?;
                Ok(std::iter::once(workspace)
                    .chain(state.roots.iter().map(|root| root.subtree.clone()))
                    .collect())
            } => result,
        }
    }

    async fn discover(&self, candidate: &Path) -> ProbeResult {
        let mut cached = self.source.lock().await;
        if cached.is_none() {
            let workspace = self.workspace.clone();
            let workspace =
                blocking(move || canonical_prefix(workspace).map_err(HostError::failed)).await?;
            let existing = workspace.clone();
            let existing = blocking(move || existing_directory(&existing)).await?;
            let Some(source) = self.probe(&existing, None).await? else {
                return Ok(None);
            };
            let relative = workspace
                .strip_prefix(&source.root)
                .map_err(|_| HostError::failed("The source workspace is outside its Git root"))?
                .to_path_buf();
            *cached = Some((source, relative));
        }
        let (source, relative) = cached
            .as_ref()
            .expect("a verified source was cached")
            .clone();
        drop(cached);
        let Some(directory) = self.probe(candidate, Some(&source.repository)).await? else {
            return Ok(None);
        };
        let subtree = directory.root.join(relative);
        blocking(move || {
            if canonical_prefix(subtree.clone()).map_err(HostError::failed)? != subtree {
                return Ok(None);
            }
            let identity = Arc::new(open_directory(&directory.root).map_err(failed)?);
            Ok(Some(Root {
                directory,
                subtree,
                identity,
            }))
        })
        .await
    }

    async fn probe(
        &self,
        cwd: &Path,
        expected: Option<&Path>,
    ) -> Result<Option<WorktreeDirectory>, HostError> {
        let Some(root) = self.git_path(cwd, "--show-toplevel").await? else {
            return Ok(None);
        };
        let repository = self
            .git_path(cwd, "--git-common-dir")
            .await?
            .ok_or_else(|| HostError::failed("Git did not report a common directory"))?;
        let repository =
            blocking(move || std::fs::canonicalize(repository).map_err(failed)).await?;
        if expected.is_some_and(|expected| expected != repository) {
            return Ok(None);
        }
        let git_dir = self
            .git_path(cwd, "--git-dir")
            .await?
            .ok_or_else(|| HostError::failed("Git did not report an administrative directory"))?;
        blocking(move || {
            let root = std::fs::canonicalize(root).map_err(failed)?;
            let git_dir = std::fs::canonicalize(git_dir).map_err(failed)?;
            let entry = root.join(".git");
            let actual = if std::fs::metadata(&entry).map_err(failed)?.is_dir() {
                std::fs::canonicalize(&entry).map_err(failed)?
            } else {
                let text = metadata_text(&entry)?;
                let value = text
                    .strip_prefix("gitdir: ")
                    .ok_or_else(|| HostError::failed("Invalid worktree gitfile"))?;
                let value = path_line(value)?;
                std::fs::canonicalize(root.join(value)).map_err(failed)?
            };
            if actual != git_dir {
                return Err(HostError::failed(
                    "Worktree Git directory does not match its gitfile",
                ));
            }
            if git_dir == repository {
                if root.join(".git") != repository {
                    return Err(HostError::failed(
                        "The main worktree relationship cannot be verified",
                    ));
                }
            } else {
                if git_dir.parent() != Some(repository.join("worktrees").as_path()) {
                    return Err(HostError::failed(
                        "Invalid linked worktree administrative directory",
                    ));
                }
                let back = path_line(&metadata_text(&git_dir.join("gitdir"))?)?;
                let back = git_dir.join(back);
                if back.file_name() != Some(std::ffi::OsStr::new(".git"))
                    || back
                        .parent()
                        .and_then(|parent| std::fs::canonicalize(parent).ok())
                        .as_ref()
                        != Some(&root)
                {
                    return Err(HostError::failed(
                        "Worktree Git backlink names another root",
                    ));
                }
                if std::fs::canonicalize(back).map_err(failed)?
                    != std::fs::canonicalize(&entry).map_err(failed)?
                {
                    return Err(HostError::failed("Worktree Git backlink does not match"));
                }
                let common = path_line(&metadata_text(&git_dir.join("commondir"))?)?;
                if std::fs::canonicalize(git_dir.join(common)).map_err(failed)? != repository {
                    return Err(HostError::failed(
                        "Worktree common directory does not match",
                    ));
                }
            }
            Ok(Some(WorktreeDirectory { repository, root }))
        })
        .await
    }

    async fn git_path(&self, cwd: &Path, option: &str) -> Result<Option<PathBuf>, HostError> {
        if self.stopping.is_cancelled() || self.lifecycle.phase() != SessionPhase::Active {
            return Err(cancelled());
        }
        let mut env: Vec<_> = self
            .platform
            .env
            .iter()
            .filter(|(key, _)| {
                !key.to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("GIT_")
            })
            .cloned()
            .collect();
        env.retain(|(key, _)| key != "LC_ALL");
        env.push(("LC_ALL".into(), "C".into()));
        let mut tree = self
            .platform
            .spawn(ProcessSpec {
                executable: "git".into(),
                args: vec![
                    "-C".into(),
                    cwd.as_os_str().to_owned(),
                    "rev-parse".into(),
                    "--path-format=absolute".into(),
                    option.into(),
                ],
                cwd: cwd.to_path_buf(),
                env,
            })
            .map_err(failed)?;
        let mut stdout = tree.stdout().take(METADATA_LIMIT + 1);
        let mut stderr = tree.stderr().take(16 * 1024);
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let read = async {
            tokio::try_join!(
                stdout.read_to_end(&mut output),
                stderr.read_to_end(&mut errors)
            )
            .map_err(failed)?;
            if output.len() as u64 > METADATA_LIMIT {
                return Err(HostError::failed("Git metadata exceeds the path limit"));
            }
            tree.wait().await.map_err(failed)
        };
        let status = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(3), read) => match result {
                Ok(status) => status,
                Err(_) => { tree.terminate().await.map_err(failed)?; return Err(HostError::failed("Git worktree discovery timed out")); }
            },
            _ = self.stopping.cancelled() => { tree.terminate().await.map_err(failed)?; return Err(cancelled()); },
            _ = self.lifecycle.closing() => { tree.terminate().await.map_err(failed)?; return Err(cancelled()); },
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                tree.terminate().await.map_err(failed)?;
                return Err(error);
            }
        };
        if !status.success() {
            let diagnostic = String::from_utf8_lossy(&errors);
            if diagnostic.contains("not a git repository")
                || diagnostic.contains("must be run in a work tree")
            {
                return Ok(None);
            }
            return Err(HostError::failed(format!(
                "Git rejected worktree metadata: {status}"
            )));
        }
        let text = String::from_utf8(output).map_err(failed)?;
        let path = path_line(&text)?;
        if !path.is_absolute() {
            return Err(HostError::failed("Git returned a relative directory"));
        }
        Ok(Some(path))
    }
}

fn metadata_text(path: &Path) -> Result<String, HostError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(failed)?;
    let metadata = file.metadata().map_err(failed)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(HostError::failed("Git metadata is not a regular file"));
    }
    let mut text = String::new();
    file.take(METADATA_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(failed)?;
    if text.len() as u64 > METADATA_LIMIT {
        return Err(HostError::failed("Git metadata exceeds the path limit"));
    }
    Ok(text)
}
fn path_line(text: &str) -> Result<PathBuf, HostError> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    #[cfg(windows)]
    let text = text.strip_suffix('\r').unwrap_or(text);
    if text.is_empty() || text.contains(['\n', '\r', '\0']) {
        return Err(HostError::failed(
            "Git path cannot be represented without ambiguity",
        ));
    }
    Ok(PathBuf::from(text))
}
fn existing_directory(path: &Path) -> Result<PathBuf, HostError> {
    let mut path = path;
    loop {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => return Ok(path.to_path_buf()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(error)),
        }
        path = path
            .parent()
            .ok_or_else(|| HostError::failed("No existing directory for worktree discovery"))?;
    }
}
/// The wire form of one association: its three paths as UTF-8 strings.
fn association(session: &str, root: &Root) -> Result<WorktreeAssociation, HostError> {
    let path = |path: &Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            HostError::failed(format!("worktree path is not UTF-8: {}", path.display()))
        })
    };
    Ok(WorktreeAssociation {
        session_id: session.to_owned(),
        repository: path(&root.directory.repository)?,
        root: path(&root.directory.root)?,
        subtree: path(&root.subtree)?,
    })
}
fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}
#[cfg(unix)]
fn identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(windows)]
fn identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the file owns a live handle and info is writable for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized info.
    let info = unsafe { info.assume_init() };
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
fn failed(error: impl std::fmt::Display) -> HostError {
    HostError::failed(format!("Worktree discovery unavailable: {error}"))
}
fn cancelled() -> HostError {
    HostError::Cancelled("Worktree discovery cancelled".into())
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, HostError> + Send + 'static,
) -> Result<T, HostError> {
    tokio::task::spawn_blocking(work).await.map_err(failed)?
}
