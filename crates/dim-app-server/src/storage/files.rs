//! SessionStore's Session files ([design](../../../../docs/design/appserver/session-files.md)):
//! immutable files at `<db>.session-files/<fileId>/<name>` on the Session's
//! resource node, owned by `session_file_owners` rows in the native store.
//!
//! A put is two write transactions around the file write. Txn 1 records the
//! file as pending under this SessionStore generation `G`; txn 2 makes it
//! owned only while the Session exists and the row is still pending under
//! `G`. SQLite orders both against S5c's fork and delete, so no in-memory
//! lock is needed.
//!
//! A background sweep removes what no row owns: rows pending under a dead
//! generation, which it claims first, unowned rows, and directory entries
//! with no row at all, such as a directory a power loss restored after its
//! row was deleted.
use super::{SqliteStorage, sqlite::error};
use dim_agent_capabilities::{
    file_storage::{FileStorage, FileStorageError},
    host::{SessionFileError, SessionFileErrorCode, SessionFilesService, StoredSessionFile},
    host_platform::HostPlatform,
};
use rusqlite::{Connection, TransactionBehavior, params};
use rutis::Ctx;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::Permissions,
    future::Future,
    ops::ControlFlow,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// One file's limit: a put is one message, and storage holds it in memory.
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The `sessionFiles` service of one SessionStore generation.
#[derive(Clone)]
pub(super) struct SessionFiles(Arc<Files>);
struct Files {
    db: Arc<SqliteStorage>,
    /// SessionStore's own context: each call reads the node's FileStorage,
    /// so a FileStorage reload reloads nothing above it.
    ctx: Ctx,
    /// The FileStorage root of the database directory.
    root: usize,
    /// `<db>.session-files`, the directory below that root.
    directory: OsString,
    /// This generation's writer id.
    generation: String,
    /// Schedules a sweep; a request during a sweep schedules one more pass.
    sweep: Arc<Notify>,
    /// A sweep of this generation reached every row and entry. Until then
    /// every put schedules one, so a sweep stopped by an unavailable node is
    /// retried by the next put.
    swept: AtomicBool,
    admission: Mutex<()>,
    stopping: CancellationToken,
    tasks: TaskTracker,
}

fn file_error(code: SessionFileErrorCode, message: impl std::fmt::Display) -> SessionFileError {
    SessionFileError {
        code,
        message: message.to_string(),
    }
}
fn cancelled() -> SessionFileError {
    file_error(
        SessionFileErrorCode::Cancelled,
        "Session file put was cancelled",
    )
}
fn not_found() -> SessionFileError {
    file_error(SessionFileErrorCode::NotFound, "Session not found")
}
fn node_unavailable() -> SessionFileError {
    file_error(
        SessionFileErrorCode::NodeUnavailable,
        "the Session's resource node storage is unavailable",
    )
}
/// A FileStorage failure while the file is written.
fn storage_failure(failure: FileStorageError) -> SessionFileError {
    match failure {
        FileStorageError::Closed => node_unavailable(),
        FileStorageError::Cancelled => cancelled(),
        other => file_error(SessionFileErrorCode::Storage, other),
    }
}

/// `[A-Za-z0-9._-]{1,64}` without a leading dot, never FileStorage's account
/// file or marker in any letter case: `auth.json` would switch replacement to
/// the account-marker protocol.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !name.eq_ignore_ascii_case("auth.json")
        && !name.eq_ignore_ascii_case("auth.json.lock")
}

/// Written once, read-only: a guard against `write` and `edit`, not isolation.
#[cfg(unix)]
fn read_only(_: &Path) -> std::io::Result<Permissions> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Permissions::from_mode(0o400))
}
/// Windows permissions carry file attributes and come only from metadata; the
/// database file is a regular file in the same root.
#[cfg(not(unix))]
fn read_only(database: &Path) -> std::io::Result<Permissions> {
    let mut permissions = std::fs::metadata(database)?.permissions();
    permissions.set_readonly(true);
    Ok(permissions)
}

fn session_exists(connection: &Connection, session_id: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id = ?1)",
        [session_id],
        |row| row.get(0),
    )
}

/// The resolved form used by registration: the longest existing prefix is
/// canonicalized and the remaining normal components are appended. A relative
/// path, `.`, `..`, or an unresolvable prefix has no form (ADR 013 D3).
fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || has_relative_component(path) {
        return None;
    }
    let mut suffix: Vec<OsString> = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut resolved) => {
                for name in suffix.iter().rev() {
                    resolved.push(name);
                }
                return Some(resolved);
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                let name = current.file_name()?;
                if Path::new(name)
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                {
                    return None;
                }
                suffix.push(name.to_os_string());
                current = current.parent()?;
            }
            Err(_) => return None,
        }
    }
}

/// The canonical form of an existing path that reaches its target without a
/// symlink. `owns` uses it: a `..` or a symlinked component has no owned form
/// (ADR 013 D3). Registration stores the canonical form, so `owns` receives
/// one from a caller that resolved the path first.
fn owned_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || has_relative_component(path) {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    (canonical == path).then_some(canonical)
}

fn has_relative_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

/// `path` followed by `separator` and `%`, escaped for a `LIKE` pattern with
/// `ESCAPE '\'`. The separator is escaped like any other literal: on Windows
/// it is `\`, which would otherwise escape the wildcard (ADR 013 D4).
fn like_prefix(path: &Path, separator: char) -> String {
    let mut prefix = String::new();
    for character in path.to_string_lossy().chars() {
        push_like_literal(&mut prefix, character);
    }
    push_like_literal(&mut prefix, separator);
    prefix.push('%');
    prefix
}

fn push_like_literal(pattern: &mut String, character: char) {
    if matches!(character, '\\' | '%' | '_') {
        pattern.push('\\');
    }
    pattern.push(character);
}

/// How txn 2 ended.
enum Commit {
    Owned,
    SessionGone,
    /// A sweep of a later generation claimed the row.
    Superseded,
}

/// S5c's fork step, run inside the fork's own transaction: the fork owns
/// every file its source owns. No file is touched, so a fork succeeds while
/// the node is offline.
pub(super) fn share(connection: &Connection, source: &str, fork: &str) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO session_file_owners (session_id, file_id) SELECT ?2, file_id FROM session_file_owners WHERE session_id = ?1",
        params![source, fork],
    )
}
/// ADR 013 D4: the fork copies the source's output rows in its transaction.
/// A registered path need not exist yet: `run_background` registers an output
/// path before the command writes it.
pub(super) fn share_output(
    connection: &Connection,
    source: &str,
    fork: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO session_output_owners (session_id, path) SELECT ?2, path FROM session_output_owners WHERE session_id = ?1",
        params![source, fork],
    )
}
/// The delete and creation-rollback step, run inside their transaction
/// before the Session's own rows: the Session stops owning its files. After
/// the commit, a file whose last owner it was is unowned and the caller
/// schedules a sweep. Returns the number of released files.
pub(super) fn release(connection: &Connection, session_id: &str) -> rusqlite::Result<usize> {
    connection.execute(
        "DELETE FROM session_file_owners WHERE session_id = ?1",
        [session_id],
    )
}

impl SessionFiles {
    pub(super) fn new(db: Arc<SqliteStorage>, ctx: Ctx, root: usize, sweep: Arc<Notify>) -> Self {
        let mut directory = db
            .path
            .file_name()
            .expect("the native store path names a file")
            .to_os_string();
        directory.push(".session-files");
        Self(Arc::new(Files {
            db,
            ctx,
            root,
            directory,
            generation: uuid::Uuid::new_v4().to_string(),
            sweep,
            swept: AtomicBool::new(false),
            admission: Mutex::new(()),
            stopping: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }))
    }

    /// Starts this generation's sweeper, which sweeps the control node
    /// whenever a sweep is scheduled. At most one sweep runs at a time.
    pub(super) fn start(&self) {
        let files = self.0.clone();
        self.0.tasks.spawn(async move {
            loop {
                tokio::select! {
                    () = files.stopping.cancelled() => return,
                    () = files.sweep.notified() => {}
                }
                if files.sweep().await {
                    files.swept.store(true, Ordering::Release);
                }
            }
        });
    }

    /// Cancels admitted puts and the sweep, and waits until each put has
    /// committed or abandoned its file. Removal is retry-safe, so a sweep
    /// cut short is resumed by a later one.
    pub(super) async fn close(&self) {
        {
            let _admission = self.0.admission.lock().unwrap();
            self.0.stopping.cancel();
            self.0.tasks.close();
        }
        self.0.tasks.wait().await;
    }

    /// S5c's delete step: removes every output directory the Session needs
    /// (ADR 013 D4): its own `<database directory>/command-output/<session>`
    /// and the directory of every path it has a row for, except one another
    /// Session of this database still names a path under. An absent directory
    /// counts as removed. Each removal is durable before the caller commits
    /// the row deletion. Fails without the node's FileStorage, without
    /// HostPlatform for a shell directory, or with `Locked` or
    /// `DirectoryNotEmpty` while a writer outside the closed Session holds
    /// an entry; the delete is then retried.
    pub(super) async fn remove_output(&self, session_id: &str) -> dim_agent_runtime::Result<()> {
        let session = session_id.to_owned();
        let own = self
            .0
            .db
            .path
            .parent()
            .map(|directory| {
                directory
                    .join(super::COMMAND_OUTPUT_DIRECTORY)
                    .join(session_id)
            })
            .ok_or_else(|| error("the native store path has no directory"))?;
        let candidates = self
            .0
            .db
            .run(move |connection| {
                let mut candidates = BTreeSet::from([own]);
                let mut query = connection
                    .prepare("SELECT DISTINCT path FROM session_output_owners WHERE session_id = ?1")
                    .map_err(error)?;
                let rows = query
                    .query_map([&session], |row| row.get::<_, String>(0))
                    .map_err(error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(error)?;
                for path in rows {
                    if let Some(directory) = Path::new(&path).parent() {
                        candidates.insert(directory.to_path_buf());
                    }
                }
                let mut kept = Vec::new();
                for candidate in candidates {
                    let text = candidate.to_string_lossy().into_owned();
                    let shared: bool = connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM session_output_owners WHERE session_id <> ?1 AND (path = ?2 OR path LIKE ?3 ESCAPE '\\'))",
                            params![session, text, like_prefix(&candidate, std::path::MAIN_SEPARATOR)],
                            |row| row.get(0),
                        )
                        .map_err(error)?;
                    if !shared {
                        kept.push(candidate);
                    }
                }
                Ok(kept)
            })
            .await?;
        let storage = self
            .0
            .ctx
            .get::<FileStorage>()
            .ok_or_else(|| error("the Session's resource node storage is unavailable"))?;
        let host = self.0.ctx.get::<HostPlatform>();
        for candidate in candidates {
            if let Some(host) = host
                .as_ref()
                .filter(|host| candidate.starts_with(host.exec_root()))
            {
                let path = candidate.clone();
                let host = host.clone();
                tokio::task::spawn_blocking(move || host.remove_exec_directory(&path))
                    .await
                    .map_err(error)?
                    .map_err(error)?;
                continue;
            }
            let relative = candidate
                .strip_prefix(storage.root_path(self.0.root).map_err(error)?)
                .map_err(|_| {
                    error(format!(
                        "output directory {} is outside the resource node's roots",
                        candidate.display()
                    ))
                })?;
            if !relative.starts_with(super::COMMAND_OUTPUT_DIRECTORY)
                || relative == Path::new(super::COMMAND_OUTPUT_DIRECTORY)
            {
                return Err(error(format!(
                    "output directory {} is not a command output directory",
                    candidate.display()
                )));
            }
            let path = storage.path(self.0.root, relative).map_err(error)?;
            storage
                .remove(&path, &self.0.stopping)
                .await
                .map_err(error)?;
        }
        #[cfg(test)]
        tests::after_output_removal(session_id).await?;
        Ok(())
    }

    /// ADR 013 D2: records `path` as output owned by `session_id` before the
    /// tool result hands it to the model. A repeated path is a no-op; a
    /// missing Session fails.
    pub(super) async fn register(
        &self,
        session_id: String,
        path: PathBuf,
    ) -> Result<(), SessionFileError> {
        let resolved = resolve(&path).ok_or_else(|| {
            file_error(
                SessionFileErrorCode::Invalid,
                "an output path must be absolute and resolvable",
            )
        })?;
        let path = resolved.into_os_string().into_string().map_err(|_| {
            file_error(
                SessionFileErrorCode::Invalid,
                "an output path must be UTF-8",
            )
        })?;
        let session = session_id.clone();
        let registered = self
            .0
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                if !session_exists(&tx, &session).map_err(error)? {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO session_output_owners (session_id, path) VALUES (?1, ?2) ON CONFLICT(session_id, path) DO NOTHING",
                    params![session, path],
                )
                .map_err(error)?;
                tx.commit().map_err(error)?;
                Ok(true)
            })
            .await
            .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))?;
        if registered { Ok(()) } else { Err(not_found()) }
    }

    /// ADR 013 D3: resolves `path` and answers from both ownership tables.
    /// An exact output row owns its path; a Session file path is owned while
    /// it stays inside `<db>.session-files/<fileId>/` of an owned file id.
    /// An unresolvable path is unowned.
    pub(super) async fn owns(
        &self,
        session_id: String,
        path: PathBuf,
    ) -> Result<bool, SessionFileError> {
        if path.components().any(|component| {
            !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        }) {
            return Ok(false);
        }
        if let Some(output) = owned_path(&path) {
            let id = session_id.clone();
            let text = output.to_string_lossy().into_owned();
            let owned = self
                .0
                .db
                .run(move |connection| {
                    connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM session_output_owners WHERE session_id = ?1 AND path = ?2)",
                            params![id, text],
                            |row| row.get(0),
                        )
                        .map_err(error)
                })
                .await
                .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))?;
            if owned {
                return Ok(true);
            }
        }

        let storage = self
            .0
            .ctx
            .get::<FileStorage>()
            .ok_or_else(node_unavailable)?;
        let directory = storage
            .root_path(self.0.root)
            .map_err(storage_failure)?
            .join(&self.0.directory);
        let directory = match tokio::fs::canonicalize(&directory).await {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(file_error(SessionFileErrorCode::Storage, error)),
        };
        let Some(rest) = path.strip_prefix(&directory).ok() else {
            return Ok(false);
        };
        let Some(Component::Normal(name)) = rest.components().next() else {
            return Ok(false);
        };
        let Some(file_id) = name.to_str() else {
            return Ok(false);
        };
        let file_id = file_id.to_owned();
        self.0
            .db
            .run(move |connection| {
                connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM session_file_owners WHERE session_id = ?1 AND file_id = ?2)",
                        params![session_id, file_id],
                        |row| row.get(0),
                    )
                    .map_err(error)
            })
            .await
            .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))
    }

    /// ADR 013 D3: the directory of every Session file the Session owns and
    /// every output path it owns.
    pub(super) async fn owned_paths(
        &self,
        session_id: String,
    ) -> Result<Vec<PathBuf>, SessionFileError> {
        let directory = self.session_files_directory();
        let (files, outputs) = self
            .0
            .db
            .run(move |connection| {
                let files: Vec<String> = connection
                    .prepare("SELECT file_id FROM session_file_owners WHERE session_id = ?1 ORDER BY file_id")
                    .map_err(error)?
                    .query_map([&session_id], |row| row.get(0))
                    .map_err(error)?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(error)?;
                let outputs: Vec<String> = connection
                    .prepare("SELECT path FROM session_output_owners WHERE session_id = ?1 ORDER BY path")
                    .map_err(error)?
                    .query_map([&session_id], |row| row.get(0))
                    .map_err(error)?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(error)?;
                Ok((files, outputs))
            })
            .await
            .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))?;
        Ok(files
            .into_iter()
            .map(|file_id| directory.join(file_id))
            .chain(outputs.into_iter().map(PathBuf::from))
            .collect())
    }

    fn session_files_directory(&self) -> PathBuf {
        self.0.db.path.with_file_name(&self.0.directory)
    }
}

impl SessionFilesService for SessionFiles {
    fn put(
        &self,
        session_id: String,
        name: String,
        content: Vec<u8>,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<StoredSessionFile, SessionFileError>> + Send + '_>>
    {
        Box::pin(async move {
            if !valid_name(&name) {
                return Err(file_error(
                    SessionFileErrorCode::Invalid,
                    "a Session file name is 1 to 64 of A-Z, a-z, 0-9, '.', '_' and '-', without a leading dot, and not auth.json",
                ));
            }
            if content.len() > MAX_BYTES {
                return Err(file_error(
                    SessionFileErrorCode::TooLarge,
                    "a Session file holds at most 16 MiB",
                ));
            }
            let call = cancel.child_token();
            let task = {
                let _admission = self.0.admission.lock().unwrap();
                if self.0.stopping.is_cancelled() {
                    return Err(cancelled());
                }
                let files = self.0.clone();
                let call = call.clone();
                self.0
                    .tasks
                    .spawn(async move { files.put(session_id, name, content, call).await })
            };
            let _dropped = call.drop_guard();
            task.await
                .unwrap_or_else(|join| Err(file_error(SessionFileErrorCode::Storage, join)))
        })
    }

    fn register(
        &self,
        session_id: String,
        path: PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionFileError>> + Send + '_>> {
        Box::pin(SessionFiles::register(self, session_id, path))
    }

    fn owns(
        &self,
        session_id: String,
        path: PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SessionFileError>> + Send + '_>> {
        Box::pin(SessionFiles::owns(self, session_id, path))
    }

    fn owned_paths(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PathBuf>, SessionFileError>> + Send + '_>> {
        Box::pin(SessionFiles::owned_paths(self, session_id))
    }
}

impl Files {
    async fn put(
        self: Arc<Self>,
        session_id: String,
        name: String,
        content: Vec<u8>,
        cancel: CancellationToken,
    ) -> Result<StoredSessionFile, SessionFileError> {
        let stopped = || cancel.is_cancelled() || self.stopping.is_cancelled();
        if stopped() {
            return Err(cancelled());
        }
        if !self.swept.load(Ordering::Acquire) {
            self.sweep.notify_one();
        }
        let storage = self.ctx.get::<FileStorage>().ok_or_else(node_unavailable)?;
        let file_id = uuid::Uuid::new_v4().to_string();
        let file_directory = Path::new(&self.directory).join(&file_id);
        let paths = (|| {
            Ok::<_, FileStorageError>((
                storage.path(self.root, &self.directory)?,
                storage.path(self.root, &file_directory)?,
                storage.path(self.root, file_directory.join(&name))?,
                storage.root_path(self.root)?,
            ))
        })();
        let (directory, file_path, file, root) = match paths {
            Ok(paths) => paths,
            Err(FileStorageError::Closed) => return Err(node_unavailable()),
            Err(other) => return Err(file_error(SessionFileErrorCode::Invalid, other)),
        };
        let permissions = read_only(&self.db.path)
            .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))?;

        let (session, id, generation) =
            (session_id.clone(), file_id.clone(), self.generation.clone());
        let pending = self
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                if !session_exists(&tx, &session).map_err(error)? {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO session_files (file_id, node, writer) VALUES (?1, NULL, ?2)",
                    params![id, generation],
                )
                .map_err(error)?;
                tx.commit().map_err(error)?;
                Ok(true)
            })
            .await
            .map_err(|failure| file_error(SessionFileErrorCode::Storage, failure))?;
        if !pending {
            return Err(not_found());
        }

        let bytes = content.len() as u64;
        let written = async {
            storage.create_directory(&directory, &cancel).await?;
            storage.create_directory(&file_path, &cancel).await?;
            storage
                .replace(&file, content, Some(permissions), &cancel)
                .await
        }
        .await;
        if let Err(failure) = written {
            self.abandon(&file_id).await;
            return Err(storage_failure(failure));
        }
        #[cfg(test)]
        tests::before_commit(&session_id);
        if stopped() {
            self.abandon(&file_id).await;
            return Err(cancelled());
        }

        let (session, id, generation) = (session_id, file_id.clone(), self.generation.clone());
        let committed = self
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                if !session_exists(&tx, &session).map_err(error)? {
                    return Ok(Commit::SessionGone);
                }
                if tx
                    .execute(
                        "UPDATE session_files SET writer = NULL WHERE file_id = ?1 AND writer = ?2",
                        params![id, generation],
                    )
                    .map_err(error)?
                    == 0
                {
                    return Ok(Commit::Superseded);
                }
                tx.execute(
                    "INSERT INTO session_file_owners (session_id, file_id) VALUES (?1, ?2)",
                    params![session, id],
                )
                .map_err(error)?;
                tx.commit().map_err(error)?;
                Ok(Commit::Owned)
            })
            .await;
        match committed {
            Ok(Commit::Owned) => Ok(StoredSessionFile {
                path: root.join(&file_directory).join(&name),
                file_id,
                bytes,
            }),
            Ok(Commit::SessionGone) => {
                self.abandon(&file_id).await;
                Err(not_found())
            }
            Ok(Commit::Superseded) => Err(cancelled()),
            Err(failure) => {
                self.abandon(&file_id).await;
                Err(file_error(SessionFileErrorCode::Storage, failure))
            }
        }
    }

    /// Makes a pending file of this generation unowned. If that fails too,
    /// the row stays pending under this generation and the next one sweeps it.
    async fn abandon(&self, file_id: &str) {
        let (id, generation) = (file_id.to_owned(), self.generation.clone());
        let abandoned = self
            .db
            .run(move |connection| {
                connection
                    .execute(
                        "UPDATE session_files SET writer = NULL WHERE file_id = ?1 AND writer = ?2",
                        params![id, generation],
                    )
                    .map_err(error)
            })
            .await;
        match abandoned {
            Ok(_) => self.sweep.notify_one(),
            Err(failure) => {
                tracing::warn!(file_id, error = %failure, "Session file left pending for the next generation");
            }
        }
    }

    /// One sweep of the control node, whose rows have a NULL node. False if
    /// it stopped early because the node's storage is unavailable or the
    /// generation is unloading; the next sweep resumes. Rows and entries it
    /// cannot remove now are kept for the next sweep.
    async fn sweep(&self) -> bool {
        let Some(storage) = self.ctx.get::<FileStorage>() else {
            return false;
        };
        let cancel = &self.stopping;
        let rows = |sql: &'static str| {
            self.db.run(move |connection| {
                connection
                    .prepare(sql)
                    .and_then(|mut query| {
                        query
                            .query_map([], |row| row.get(0))?
                            .collect::<rusqlite::Result<Vec<String>>>()
                    })
                    .map_err(error)
            })
        };

        let generation = self.generation.clone();
        let claimed = self
            .db
            .run(move |connection| {
                connection
                    .execute(
                        "UPDATE session_files SET writer = NULL WHERE node IS NULL AND writer IS NOT NULL AND writer <> ?1",
                        [generation],
                    )
                    .map_err(error)
            })
            .await;
        if let Err(failure) = claimed {
            tracing::warn!(error = %failure, "Session file sweep could not claim pending files");
            return false;
        }

        let unowned = match rows(
            "SELECT file_id FROM session_files f WHERE node IS NULL AND writer IS NULL AND NOT EXISTS (SELECT 1 FROM session_file_owners o WHERE o.file_id = f.file_id)",
        )
        .await
        {
            Ok(unowned) => unowned,
            Err(failure) => {
                tracing::warn!(error = %failure, "Session file sweep could not read unowned files");
                return false;
            }
        };
        for file_id in unowned {
            if self
                .remove(&storage, Path::new(&file_id), cancel)
                .await
                .is_break()
            {
                return false;
            }
            let id = file_id.clone();
            let deleted = self
                .db
                .run(move |connection| {
                    connection
                        .execute(
                            "DELETE FROM session_files WHERE file_id = ?1 AND writer IS NULL AND NOT EXISTS (SELECT 1 FROM session_file_owners WHERE file_id = ?1)",
                            [id],
                        )
                        .map_err(error)
                })
                .await;
            if let Err(failure) = deleted {
                tracing::warn!(file_id, error = %failure, "Session file row kept for the next sweep");
            }
        }

        let directory = match storage.path(self.root, &self.directory) {
            Ok(directory) => directory,
            Err(_) => return false,
        };
        let entries = match storage.list(&directory, cancel).await {
            Ok(entries) => entries,
            Err(FileStorageError::Io(failure))
                if failure.kind() == std::io::ErrorKind::NotFound =>
            {
                return true;
            }
            Err(failure) => {
                tracing::warn!(error = %failure, "Session file sweep could not list its directory");
                return false;
            }
        };
        let names: Vec<OsString> = entries.into_iter().map(|entry| entry.name).collect();
        let listed = names.clone();
        let recorded = self
            .db
            .run(move |connection| {
                let mut query = connection
                    .prepare("SELECT EXISTS(SELECT 1 FROM session_files WHERE file_id = ?1)")
                    .map_err(error)?;
                listed
                    .iter()
                    .map(|name| match name.to_str() {
                        Some(name) => query.query_row([name], |row| row.get(0)).map_err(error),
                        None => Ok(false),
                    })
                    .collect::<dim_agent_runtime::Result<Vec<bool>>>()
            })
            .await;
        let recorded = match recorded {
            Ok(recorded) => recorded,
            Err(failure) => {
                tracing::warn!(error = %failure, "Session file sweep could not read its rows");
                return false;
            }
        };
        for (name, recorded) in names.into_iter().zip(recorded) {
            if !recorded
                && self
                    .remove(&storage, Path::new(&name), cancel)
                    .await
                    .is_break()
            {
                return false;
            }
        }
        true
    }

    /// Removes `<db>.session-files/<name>`; an absent entry counts as
    /// removed. Breaks when the sweep must stop: the node's storage closed or
    /// the generation is unloading. Other failures, such as `Locked`, keep the
    /// entry for the next sweep.
    async fn remove(
        &self,
        storage: &Arc<FileStorage>,
        name: &Path,
        cancel: &CancellationToken,
    ) -> ControlFlow<()> {
        let removed = match storage.path(self.root, Path::new(&self.directory).join(name)) {
            Ok(path) => storage.remove(&path, cancel).await,
            Err(failure) => Err(failure),
        };
        match removed {
            Ok(()) => ControlFlow::Continue(()),
            Err(FileStorageError::Closed | FileStorageError::Cancelled) => ControlFlow::Break(()),
            Err(failure) => {
                tracing::warn!(entry = %name.display(), error = %failure, "Session file kept for the next sweep");
                ControlFlow::Continue(())
            }
        }
    }
}

#[cfg(test)]
mod tests;
