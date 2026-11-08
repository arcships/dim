//! Generic, host-owned file transactions for an explicitly authorized plugin root.
//! No account formats or provider policy belong at this boundary.
use dim_agent_capabilities::file_storage::{
    DirectoryEntry, EntryKind, FileLease, FilePath, FileReader, FileStorage, FileStorageError,
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, host_key};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::Permissions,
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const FORMAT_VERSION: u32 = 1;

/// The largest file a whole-buffer operation accepts. Bytes cross the plugin
/// runtime's channel base64-encoded in a 16 MiB frame, which costs four bytes
/// for every three; 8 MiB leaves the frame envelope and both sides' copies
/// room, and bounds one allocation. Streaming operations have no such limit.
pub const MAX_WHOLE_FILE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredFile {
    version: u32,
    text: String,
}

fn failure(error: FileStorageError) -> Value {
    let code = match error {
        FileStorageError::Closed | FileStorageError::Cancelled => "closed",
        FileStorageError::InvalidPath(_) => "invalid_path",
        FileStorageError::Locked => "locked",
        FileStorageError::Worker(_) => "internal",
        FileStorageError::Io(ref error) if error.kind() == io::ErrorKind::AlreadyExists => "exists",
        FileStorageError::Io(ref error) if error.kind() == io::ErrorKind::InvalidData => {
            "invalid_data"
        }
        _ => "io",
    };
    json!({"ok":false,"error":code}).into()
}
fn success(value: Value) -> Value {
    Value::Record([("ok".into(), json!(true).into()), ("value".into(), value)].into())
}
fn invalid() -> Error {
    Error::Value("invalid file operation".into())
}
fn private_permissions() -> Option<Permissions> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        None
    }
}
async fn resolve(value: Value) -> Reply {
    match value {
        Value::Reference(reference) if reference.is_future() => reference.wait_async().await,
        value => Ok(value),
    }
}

struct Files {
    storage: Arc<FileStorage>,
    root: usize,
    admission: Mutex<()>,
    stopping: CancellationToken,
    tasks: TaskTracker,
}
struct Scope {
    files: Arc<Files>,
    stopping: CancellationToken,
    tasks: TaskTracker,
}
impl Scope {
    fn quiesce(&self) {
        let _admission = self.files.admission.lock().unwrap();
        self.stopping.cancel();
        self.tasks.close();
    }
    fn value(self: Arc<Self>) -> Value {
        let transaction = self.clone();
        let listing = self.clone();
        let removal = self.clone();
        let quiesce = self.clone();
        Value::Record(
            [
                (
                    "withLock".into(),
                    Value::callback(move |args| transaction.with_lock(args)),
                ),
                (
                    "list".into(),
                    Value::callback(move |args| listing.list(args)),
                ),
                (
                    "remove".into(),
                    Value::callback(move |args| removal.remove(args)),
                ),
                (
                    "quiesce".into(),
                    Value::callback(move |_| {
                        quiesce.quiesce();
                        Ok(json!(null).into())
                    }),
                ),
                (
                    "close".into(),
                    Value::callback(move |_| {
                        self.quiesce();
                        let scope = self.clone();
                        Ok(Value::future(async move {
                            scope.tasks.wait().await;
                            Ok(json!(null).into())
                        }))
                    }),
                ),
            ]
            .into(),
        )
    }
    /// The value of the named directory's entries, one level, relative to the
    /// authorized root.
    fn list(self: &Arc<Self>, args: Value) -> Reply {
        self.operation(args, |files, prefix, path, cancel| {
            Box::pin(async move {
                Ok(match files.storage.list(&path, &cancel).await {
                    Ok(entries) => success(entries_value(Path::new(&prefix), entries)),
                    Err(error) => failure(error),
                })
            })
        })
    }
    /// The value of removing `name`, a file or a directory tree.
    fn remove(self: &Arc<Self>, args: Value) -> Reply {
        self.operation(args, |files, _, path, cancel| {
            Box::pin(async move {
                Ok(match files.storage.remove(&path, &cancel).await {
                    Ok(()) => success(json!(null).into()),
                    Err(error) => failure(error),
                })
            })
        })
    }
    /// Runs `operation` on the path the single argument of `args` names. A
    /// quiesced or closed scope refuses it; an issued operation settles before
    /// `close` resolves.
    fn operation(
        self: &Arc<Self>,
        args: Value,
        operation: impl FnOnce(
            Arc<Files>,
            String,
            FilePath,
            CancellationToken,
        ) -> BoxFuture<'static, Reply>
        + Send
        + 'static,
    ) -> Reply {
        let mut args = args.list()?.into_iter();
        let name: String = serde_json::from_value(args.next().ok_or_else(invalid)?.json()?)
            .map_err(|_| invalid())?;
        if args.next().is_some() {
            return Err(invalid());
        }
        let path = match self.files.storage.path(self.files.root, &name) {
            Ok(path) => path,
            Err(error) => return Ok(Value::future(async move { Ok(failure(error)) })),
        };
        let _admission = self.files.admission.lock().unwrap();
        if self.stopping.is_cancelled() {
            return Ok(Value::future(async {
                Ok(failure(FileStorageError::Closed))
            }));
        }
        let scope = self.clone();
        let stopping = self.stopping.child_token();
        let owner = self.files.tasks.token();
        let task = self.tasks.spawn(async move {
            let _owner = owner;
            operation(scope.files.clone(), name, path, stopping).await
        });
        Ok(Value::future(async move {
            task.await
                .unwrap_or_else(|error| Ok(failure(FileStorageError::Worker(error))))
        }))
    }
    fn with_lock(self: &Arc<Self>, args: Value) -> Reply {
        let mut args = args.list()?.into_iter();
        let name: String = serde_json::from_value(args.next().ok_or_else(invalid)?.json()?)
            .map_err(|_| invalid())?;
        let callback = args.next().ok_or_else(invalid)?.reference()?;
        let cancelled = args.next().map(Value::reference).transpose()?;
        if cancelled
            .as_ref()
            .is_some_and(|reference| !reference.is_future())
            || args.next().is_some()
        {
            return Err(invalid());
        }
        let path = match self.files.storage.path(self.files.root, name) {
            Ok(path) => path,
            Err(error) => return Ok(Value::future(async move { Ok(failure(error)) })),
        };
        let _admission = self.files.admission.lock().unwrap();
        if self.stopping.is_cancelled() {
            return Ok(Value::future(async {
                Ok(failure(FileStorageError::Closed))
            }));
        }
        let scope = self.clone();
        let acquisition = self.stopping.child_token();
        let cancel_waiter = acquisition.clone().drop_guard();
        let owner = self.files.tasks.token();
        let task = self.tasks.spawn(async move {
            let _owner = owner;
            let acquire = async {
                loop {
                    match scope.files.storage.lock(&path, &acquisition).await {
                        Ok(lease) => return Ok(lease),
                        Err(FileStorageError::Locked) => tokio::select! {
                            _ = acquisition.cancelled() => return Err(FileStorageError::Closed),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                        },
                        Err(error) => return Err(error),
                    }
                }
            };
            let cancelled = async {
                match cancelled {
                    Some(reference) => {
                        let _ = reference.wait_async().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let lease = tokio::select! {
                _ = cancelled => {
                    acquisition.cancel();
                    return Ok(failure(FileStorageError::Closed));
                }
                result = acquire => match result {
                    Ok(lease) => lease,
                    Err(error) => return Ok(failure(error)),
                }
            };
            let lease = Arc::new(tokio::sync::Mutex::new(Some(lease)));
            let result = match callback
                .call_async(Value::List(vec![lease_value(
                    lease.clone(),
                    scope.tasks.clone(),
                )]))
                .await
            {
                Ok(value) => resolve(value).await,
                Err(error) => Err(error),
            };
            lease.lock().await.take();
            result.map(success)
        });
        Ok(Value::future(async move {
            let _cancel_waiter = cancel_waiter;
            task.await
                .unwrap_or_else(|error| Ok(failure(FileStorageError::Worker(error))))
        }))
    }
}

/// One entry of the JSON `list` reply: its path relative to the authorized
/// root and its kind. An entry name that is not UTF-8 is omitted, because
/// these operations address names as strings.
fn entries_value(prefix: &Path, entries: Vec<DirectoryEntry>) -> Value {
    Value::List(
        entries
            .into_iter()
            .filter_map(|entry| {
                Some(Value::Record(
                    [
                        (
                            "name".into(),
                            json!(join_name(prefix, entry.name.to_str()?)).into(),
                        ),
                        ("kind".into(), json!(entry.kind).into()),
                    ]
                    .into(),
                ))
            })
            .collect(),
    )
}

/// The one argument a lease method takes.
enum LeaseArgument {
    None,
    Text(String),
    Bytes(Vec<u8>),
}

fn lease_value(lease: Arc<tokio::sync::Mutex<Option<FileLease>>>, tasks: TaskTracker) -> Value {
    Value::Record(
        ["read", "replace", "backup", "readBytes", "replaceBytes"]
            .into_iter()
            .map(|method| {
                let lease = lease.clone();
                let tasks = tasks.clone();
                (
                    method.into(),
                    Value::callback(move |args| {
                        let args = args.list()?;
                        let argument = match (method, args.as_slice()) {
                            ("read" | "readBytes", []) => LeaseArgument::None,
                            (
                                "replace" | "backup",
                                [Value::Data(serde_json::Value::String(text))],
                            ) => LeaseArgument::Text(text.clone()),
                            ("replaceBytes", [Value::Bytes(bytes)]) => {
                                LeaseArgument::Bytes(bytes.clone())
                            }
                            _ => return Err(invalid()),
                        };
                        let lease = lease.clone();
                        let task = tasks.spawn(async move {
                            let mut guard = lease.lock().await;
                            let Some(lease) = guard.as_mut() else {
                                return Ok(failure(FileStorageError::Closed));
                            };
                            let cancel = CancellationToken::new();
                            let result = match (method, argument) {
                                ("read", LeaseArgument::None) => match lease.read(&cancel).await {
                                    Ok(bytes) => match serde_json::from_slice::<StoredFile>(&bytes)
                                    {
                                        Ok(StoredFile {
                                            version: FORMAT_VERSION,
                                            text,
                                        }) => Ok(json!(text).into()),
                                        _ => Err(FileStorageError::Io(
                                            io::ErrorKind::InvalidData.into(),
                                        )),
                                    },
                                    Err(FileStorageError::Io(error))
                                        if error.kind() == io::ErrorKind::NotFound =>
                                    {
                                        Ok(json!(null).into())
                                    }
                                    Err(error) => Err(error),
                                },
                                ("readBytes", LeaseArgument::None) => {
                                    match lease.read_limited(MAX_WHOLE_FILE_BYTES, &cancel).await {
                                        Ok(bytes) => Ok(Value::Bytes(bytes)),
                                        Err(FileStorageError::Io(error))
                                            if error.kind() == io::ErrorKind::NotFound =>
                                        {
                                            Ok(json!(null).into())
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                                ("replace", LeaseArgument::Text(text)) => lease
                                    .replace(
                                        serde_json::to_vec(&StoredFile {
                                            version: FORMAT_VERSION,
                                            text,
                                        })
                                        .expect("text records contain only serializable fields"),
                                        private_permissions(),
                                        &cancel,
                                    )
                                    .await
                                    .map(|_| json!(null).into()),
                                ("replaceBytes", LeaseArgument::Bytes(bytes))
                                    if bytes.len() <= MAX_WHOLE_FILE_BYTES =>
                                {
                                    lease
                                        .replace(bytes, private_permissions(), &cancel)
                                        .await
                                        .map(|_| json!(null).into())
                                }
                                ("replaceBytes", LeaseArgument::Bytes(_)) => {
                                    Err(FileStorageError::Io(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "file exceeds the whole-buffer byte limit",
                                    )))
                                }
                                ("backup", LeaseArgument::Text(suffix)) => lease
                                    .backup(&suffix, private_permissions(), &cancel)
                                    .await
                                    .map(|_| json!(null).into()),
                                _ => unreachable!(),
                            };
                            Ok(match result {
                                Ok(value) => success(value),
                                Err(error) => failure(error),
                            })
                        });
                        Ok(Value::future(async move {
                            task.await.unwrap_or_else(|error| {
                                Ok(failure(FileStorageError::Worker(error)))
                            })
                        }))
                    }),
                )
            })
            .collect(),
    )
}

/// Why a typed file transaction did not run or did not commit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginFileError {
    #[error("the file store is closed")]
    Closed,
    #[error("the file path is outside the authorized root")]
    InvalidPath,
    /// The file cannot be used by this operation and is left unchanged: not a
    /// version 1 text envelope for a text transaction, or larger than
    /// [`MAX_WHOLE_FILE_BYTES`] for a byte read.
    #[error("the stored file cannot be used by this operation; it is left unchanged")]
    InvalidData,
    /// Another lease holds the file, or one beneath the directory.
    #[error("the file is locked by another lease")]
    Locked,
    #[error("file operation failed: {0}")]
    Io(String),
}

/// One entry of a [`PluginFileTransactions::list`] result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFileEntry {
    /// The entry's path relative to the authorized root, ready to pass to
    /// another operation.
    pub name: String,
    /// What the entry is, without following links.
    pub kind: EntryKind,
}

fn file_error(error: FileStorageError) -> PluginFileError {
    match error {
        FileStorageError::Closed | FileStorageError::Cancelled => PluginFileError::Closed,
        FileStorageError::InvalidPath(_) => PluginFileError::InvalidPath,
        FileStorageError::Locked => PluginFileError::Locked,
        FileStorageError::Io(ref error) if error.kind() == io::ErrorKind::InvalidData => {
            PluginFileError::InvalidData
        }
        error => PluginFileError::Io(error.to_string()),
    }
}

/// The name of `leaf` under `prefix`, as these operations spell relative
/// names: the components the storage resolved, joined by `/`, with `leaf`
/// appended. The prefix is read exactly as [`FileStorage::path`] reads it, so
/// on Unix a backslash stays part of a component and on Windows it separates;
/// no component can contain `/`, so a returned name always round-trips.
fn join_name(prefix: &Path, leaf: &str) -> String {
    let mut name = prefix
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_str()?),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if !name.is_empty() {
        name.push('/');
    }
    name.push_str(leaf);
    name
}

/// Typed transactions on files under the authorized root, for process plugins
/// written in Rust. The host's `{version, text}` envelope is read and written
/// here; a caller sees and returns only the inner text, as the JSON
/// `pluginFiles` scope exposes it.
pub struct PluginFileTransactions(Arc<Files>);
impl PluginFileTransactions {
    /// Runs `change` on the inner text of `name` under its file lock. `change`
    /// receives `None` for a missing file. It returns the replacement text
    /// (`None` keeps the file as it is) and its own result, which the
    /// transaction returns. An envelope that is not version 1 fails with
    /// [`PluginFileError::InvalidData`] before `change` runs, and the bytes are
    /// left as they were.
    pub async fn transact<R: Send + 'static>(
        &self,
        name: &str,
        change: impl FnOnce(Option<&str>) -> (Option<String>, R) + Send + 'static,
    ) -> Result<R, PluginFileError> {
        self.run(name, move |storage, path, stopping| {
            Box::pin(transact_file(storage, path, stopping, change))
        })
        .await
    }

    /// The bytes of `name`, or `None` when it is absent. A binary file is its
    /// bytes; a text file returns them whole, envelope included. A file over
    /// [`MAX_WHOLE_FILE_BYTES`] fails with [`PluginFileError::InvalidData`]
    /// before it is read; use [`open_read`](Self::open_read) for larger files.
    pub async fn read_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, PluginFileError> {
        self.run(name, |storage, path, stopping| {
            Box::pin(async move {
                match storage
                    .read_limited(&path, MAX_WHOLE_FILE_BYTES, &stopping)
                    .await
                {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        Ok(None)
                    }
                    Err(error) => Err(file_error(error)),
                }
            })
        })
        .await
    }

    /// Opens `name` for streaming reads, or `None` when it is absent. The
    /// reader pins the file: reads take explicit offsets and return at most
    /// 64 KiB each, and replacing the name does not change an open reader.
    pub async fn open_read(&self, name: &str) -> Result<Option<FileReader>, PluginFileError> {
        self.run(name, |storage, path, stopping| {
            Box::pin(async move {
                match storage.open_read(&path, &stopping).await {
                    Ok(reader) => Ok(Some(reader)),
                    Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        Ok(None)
                    }
                    Err(error) => Err(file_error(error)),
                }
            })
        })
        .await
    }

    /// Replaces `name` with what `source` yields, read to its end under the
    /// file's lock. The bytes go to a temporary file beside the target, which
    /// is flushed and renamed over it, so a failure or cancellation leaves the
    /// old content and no temporary file. No text envelope is involved, and no
    /// size bound applies.
    pub async fn write_stream(
        &self,
        name: &str,
        source: impl io::Read + Send + 'static,
    ) -> Result<(), PluginFileError> {
        self.run(name, move |storage, path, stopping| {
            Box::pin(async move {
                let mut lease = acquire(&storage, &path, &stopping).await?;
                lease
                    .replace_from(source, private_permissions(), &stopping)
                    .await
                    .map_err(file_error)
            })
        })
        .await
    }

    /// The entries directly under the directory `prefix`, sorted by name and
    /// named relative to the authorized root, so each name is ready to pass to
    /// another operation. One level: a directory entry is listed, not entered.
    /// `prefix` names a directory, so the root itself is not listed. An entry
    /// whose name is not valid UTF-8 is omitted, because these operations
    /// address names as strings; removing the directory removes it.
    pub async fn list(&self, prefix: &str) -> Result<Vec<PluginFileEntry>, PluginFileError> {
        let name = prefix.to_owned();
        let joined = name.clone();
        self.run(&name, move |storage, path, stopping| {
            Box::pin(async move {
                let entries = storage.list(&path, &stopping).await.map_err(file_error)?;
                Ok(entries
                    .into_iter()
                    .filter_map(|entry| {
                        Some(PluginFileEntry {
                            name: join_name(Path::new(&joined), entry.name.to_str()?),
                            kind: entry.kind,
                        })
                    })
                    .collect())
            })
        })
        .await
    }

    /// Removes `name`: a file, or a directory and everything beneath it. A
    /// link is removed itself, never followed. A file a lease holds, and a
    /// directory with a lease held beneath it, fail with
    /// [`PluginFileError::Locked`]; an absent entry counts as removed, so a
    /// retry is safe. Removal is not atomic: an error can leave part of a tree
    /// removed.
    pub async fn remove(&self, name: &str) -> Result<(), PluginFileError> {
        self.run(name, |storage, path, stopping| {
            Box::pin(async move { storage.remove(&path, &stopping).await.map_err(file_error) })
        })
        .await
    }

    /// Makes `name` an existing directory under the authorized root, creating
    /// every missing parent with mode 0700, and returns the absolute path it
    /// names there. It is for handing a work directory to a helper process. A
    /// link or other non-directory at any existing component fails with
    /// [`PluginFileError::InvalidPath`]; the returned path names what this call
    /// created, and a component replaced by a link afterwards is outside its
    /// control.
    pub async fn directory(&self, name: &str) -> Result<PathBuf, PluginFileError> {
        self.run(name, |storage, path, stopping| {
            Box::pin(async move {
                storage
                    .create_directories(&path, &stopping)
                    .await
                    .map_err(file_error)
            })
        })
        .await
    }

    async fn run<R: Send + 'static>(
        &self,
        name: &str,
        action: impl FnOnce(
            Arc<FileStorage>,
            FilePath,
            CancellationToken,
        ) -> BoxFuture<'static, Result<R, PluginFileError>>
        + Send
        + 'static,
    ) -> Result<R, PluginFileError> {
        let files = self.0.clone();
        let path = files
            .storage
            .path(files.root, name)
            .map_err(|_| PluginFileError::InvalidPath)?;
        let admission = files.admission.lock().unwrap();
        if files.stopping.is_cancelled() {
            return Err(PluginFileError::Closed);
        }
        let stopping = files.stopping.child_token();
        let storage = files.storage.clone();
        let owner = files.tasks.token();
        let task = files.tasks.spawn(async move {
            let _owner = owner;
            action(storage, path, stopping).await
        });
        drop(admission);
        task.await
            .unwrap_or_else(|error| Err(PluginFileError::Io(error.to_string())))
    }
}

async fn transact_file<R>(
    storage: Arc<FileStorage>,
    path: FilePath,
    stopping: CancellationToken,
    change: impl FnOnce(Option<&str>) -> (Option<String>, R),
) -> Result<R, PluginFileError> {
    let mut lease = acquire(&storage, &path, &stopping).await?;
    let current = match lease.read(&stopping).await {
        Ok(bytes) => match serde_json::from_slice::<StoredFile>(&bytes) {
            Ok(StoredFile {
                version: FORMAT_VERSION,
                text,
            }) => Some(text),
            _ => return Err(PluginFileError::InvalidData),
        },
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(file_error(error)),
    };
    let (next, result) = change(current.as_deref());
    if let Some(text) = next {
        let bytes = serde_json::to_vec(&StoredFile {
            version: FORMAT_VERSION,
            text,
        })
        .expect("text records contain only serializable fields");
        lease
            .replace(bytes, private_permissions(), &stopping)
            .await
            .map_err(file_error)?;
    }
    Ok(result)
}

/// Takes the file's lease, waiting while another holder has it.
async fn acquire(
    storage: &Arc<FileStorage>,
    path: &FilePath,
    stopping: &CancellationToken,
) -> Result<FileLease, PluginFileError> {
    loop {
        match storage.lock(path, stopping).await {
            Ok(lease) => return Ok(lease),
            Err(FileStorageError::Locked) => tokio::select! {
                _ = stopping.cancelled() => return Err(PluginFileError::Closed),
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            },
            Err(error) => return Err(file_error(error)),
        }
    }
}

struct Dispatch(Arc<Files>);
impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"open":"async"}))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "open" || !args.list()?.is_empty() {
            return Err(invalid());
        }
        let scope = Arc::new(Scope {
            files: self.0.clone(),
            stopping: self.0.stopping.child_token(),
            tasks: TaskTracker::new(),
        });
        Ok(Value::future(async move { Ok(scope.value()) }))
    }
}

/// The composition root chooses the authorized root; JS cannot register roots.
#[derive(Clone)]
pub struct PluginFilesPlugin {
    root: usize,
    storage: Key<FileStorage>,
}
impl PluginFilesPlugin {
    pub fn new(names: &Names<'_>, root: usize) -> rutis::Result<Self> {
        Ok(Self {
            root,
            storage: names.key()?,
        })
    }
}
impl DynPlugin for PluginFilesPlugin {
    fn name(&self) -> &str {
        "PluginFiles"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.storage.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let storage = ctx.require_at(&self.storage)?;
            let files = Arc::new(Files {
                storage,
                root: self.root,
                admission: Mutex::new(()),
                stopping: CancellationToken::new(),
                tasks: TaskTracker::new(),
            });
            let owner = files.clone();
            ctx.effect(move || async move {
                {
                    let _admission = owner.admission.lock().unwrap();
                    owner.stopping.cancel();
                    owner.tasks.close();
                }
                owner.tasks.wait().await;
                Ok(())
            })?;
            ctx.provide_at(
                host_key("pluginFiles"),
                Arc::new(Dispatch(files.clone())) as Arc<dyn HostDispatch>,
            )?;
            ctx.provide_as::<PluginFileTransactions>(Arc::new(PluginFileTransactions(files)))?;
            Ok(Effect::Done)
        })
    }
}
