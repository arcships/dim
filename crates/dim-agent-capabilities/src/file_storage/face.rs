use super::mentions::{MentionsReply, MentionsRequest, parse_limit};
use super::{DirectoryEntry, FileReader, FileStorage, FileStorageError, Result};
use crate::workspace_tools::files::{WorkspaceMutation, WorkspaceWrite, search::SearchKind};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis::Ctx;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRead {
    pub read_id: String,
    pub size: u64,
}

pub(super) fn serialize_name<S: serde::Serializer>(
    name: &OsStr,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match name.to_str() {
        Some(text) => serializer.serialize_str(text),
        #[cfg(any(unix, windows))]
        None => name.serialize(serializer),
        #[cfg(not(any(unix, windows)))]
        None => Err(serde::ser::Error::custom(
            "unsupported filesystem name encoding",
        )),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EntryName {
    Text(String),
    #[cfg(any(unix, windows))]
    Native(OsString),
}

fn deserialize_name<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<OsString, D::Error> {
    Ok(match EntryName::deserialize(deserializer)? {
        EntryName::Text(text) => text.into(),
        #[cfg(any(unix, windows))]
        EntryName::Native(name) => name,
    })
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum FsRemove {
    Path {
        path: PathBuf,
    },
    Entry {
        parent: PathBuf,
        #[serde(deserialize_with = "deserialize_name")]
        name: OsString,
    },
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsPermissions {
    #[default]
    Preserve,
    Private,
    PrivateReadOnly,
}

impl FsPermissions {
    fn mode(self) -> super::ReplacementMode {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            super::ReplacementMode::Native(match self {
                Self::Private => Some(std::fs::Permissions::from_mode(0o600)),
                Self::PrivateReadOnly => Some(std::fs::Permissions::from_mode(0o400)),
                Self::Preserve => None,
            })
        }
        #[cfg(not(unix))]
        match self {
            Self::PrivateReadOnly => super::ReplacementMode::ReadOnly,
            Self::Preserve | Self::Private => super::ReplacementMode::Native(None),
        }
    }
}

/// Filesystem operations on the existing storage generation.
#[derive(Clone)]
pub struct FsHost {
    pub(crate) writes: Arc<crate::workspace_tools::files::FileWriteCoordinator>,
    system_roots: Arc<Vec<PathBuf>>,
    storage: Arc<FileStorage>,
    generation: Ctx,
}

impl FsHost {
    pub(super) fn new(
        owner: Arc<FileStorage>,
        ctx: Ctx,
        writes: Arc<crate::workspace_tools::files::FileWriteCoordinator>,
    ) -> Self {
        Self {
            writes,
            system_roots: Arc::new(crate::host_platform::platform_roots()),
            storage: owner,
            generation: ctx,
        }
    }

    pub(crate) fn begin_workspace_operation(
        &self,
        cancel: &CancellationToken,
    ) -> std::result::Result<(impl Send + use<>, CancellationToken), crate::host::HostError> {
        let mut state = self.storage.state.lock().unwrap();
        self.storage.check(cancel).map_err(|error| match error {
            FileStorageError::Cancelled => crate::host::HostError::Cancelled(error.to_string()),
            error => crate::host::HostError::failed(error.to_string()),
        })?;
        state.active += 1;
        Ok((
            super::Operation(self.storage.clone()),
            self.storage.stopping.child_token(),
        ))
    }

    pub async fn mentions(
        &self,
        request: MentionsRequest,
        cancel: &CancellationToken,
    ) -> Result<MentionsReply> {
        let owner = self.storage.clone();
        if !request.path.is_absolute() {
            return Err(FileStorageError::InvalidPath(
                "expected an absolute search root",
            ));
        }
        let operation = {
            let mut state = owner.state.lock().unwrap();
            owner.check(cancel)?;
            state.active += 1;
            super::Operation(owner.clone())
        };
        let cancel = cancel.child_token();
        let _dropped = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            let candidates = owner.mentions.search(
                &request.path,
                &request.query,
                parse_limit(request.limit),
                || owner.check(&cancel),
            )?;
            Ok(MentionsReply { candidates })
        })
        .await?
    }

    /// A caller-bound lease closes on row unload. A caller managing its own
    /// settlement can bind to the file owner and explicitly unlock after settlement.
    pub async fn lock(
        &self,
        caller: Option<&Ctx>,
        path: impl AsRef<Path>,
        cancel: &CancellationToken,
    ) -> Result<Uuid> {
        let path = self.storage.resolve(path.as_ref())?;
        let lease = self.storage.lock(&path, cancel).await?;
        let id = lease.0.as_ref().expect("new file lease").id;
        let held = Arc::new(super::SharedLease::new(Some(lease)));
        let cleanup = held.clone();
        let release = caller
            .unwrap_or(&self.generation)
            .effect(move || async move {
                cleanup.lock().await.take();
                Ok(())
            })
            .map_err(|_| FileStorageError::Closed)?;
        {
            let mut state = self.storage.state.lock().unwrap();
            if let Some(entry) = state.leases.get_mut(&id) {
                entry.binding = Some((Arc::downgrade(&held), release));
                return Ok(id);
            }
        }
        let _ = release.dispose().await;
        Err(FileStorageError::Closed)
    }

    pub async fn unlock(&self, id: Uuid) -> Result<()> {
        let release = {
            let mut state = self.storage.state.lock().unwrap();
            let (_, release) = state
                .leases
                .get_mut(&id)
                .and_then(|entry| entry.binding.take())
                .ok_or(FileStorageError::InvalidPath("unknown lease id"))?;
            release
        };
        release
            .dispose()
            .await
            .map_err(|_| FileStorageError::Closed)
    }

    pub async fn stat(&self, path: &Path, cancel: &CancellationToken) -> Result<super::FileStat> {
        let path = self.storage.resolve(path)?;
        let relative = path.relative.clone();
        self.storage
            .run(&path, cancel, move |root, _, _| {
                let (parent, name) = match super::native::parent(root, &relative) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(super::FileStat::Missing);
                    }
                    result => result?,
                };
                super::native::file_stat(&parent, &name)
            })
            .await
    }

    pub async fn read_bytes(
        &self,
        path: &Path,
        limit: usize,
        lease_id: Option<Uuid>,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let path = self.storage.resolve(path)?;
        let Some(id) = lease_id else {
            return self.storage.read_limited(&path, limit, cancel).await;
        };
        self.held_lease(id, Some(&path), cancel)
            .await?
            .read_limited(limit, cancel)
            .await
    }

    async fn held_lease(
        &self,
        id: Uuid,
        path: Option<&super::FilePath>,
        cancel: &CancellationToken,
    ) -> Result<tokio::sync::OwnedMappedMutexGuard<Option<super::FileLease>, super::FileLease>>
    {
        let held = {
            let state = self.storage.state.lock().unwrap();
            state
                .leases
                .get(&id)
                .and_then(|entry| entry.binding.as_ref()?.0.upgrade())
                .ok_or(FileStorageError::InvalidPath("unknown lease id"))?
        };
        let guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(FileStorageError::Cancelled),
            guard = held.lock_owned() => guard,
        };
        let lease = tokio::sync::OwnedMutexGuard::try_map(guard, Option::as_mut)
            .map_err(|_| FileStorageError::InvalidPath("unknown lease id"))?;
        if path.is_some_and(|path| &lease.0.as_ref().expect("held file lease").path != path) {
            return Err(FileStorageError::InvalidPath(
                "lease does not hold the requested path",
            ));
        }
        Ok(lease)
    }

    pub async fn replace(
        &self,
        path: &Path,
        bytes: Vec<u8>,
        permissions: FsPermissions,
        lease_id: Option<Uuid>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let path = self.storage.resolve(path)?;
        let mode = permissions.mode();
        match lease_id {
            Some(id) => {
                self.held_lease(id, Some(&path), cancel)
                    .await?
                    .replace_with(bytes, mode, cancel)
                    .await
            }
            None => {
                self.storage
                    .lock(&path, cancel)
                    .await?
                    .replace_with(bytes, mode, cancel)
                    .await
            }
        }
    }

    /// Atomically replaces a destination from a local source without buffering the file.
    pub async fn replace_from_path(
        &self,
        path: &Path,
        source: PathBuf,
        limit: u64,
        permissions: FsPermissions,
        cancel: &CancellationToken,
    ) -> Result<u64> {
        if !source.is_absolute() {
            return Err(FileStorageError::InvalidPath(
                "expected an absolute source path",
            ));
        }
        self.storage
            .import(
                &self.storage.resolve(path)?,
                source,
                limit,
                permissions.mode(),
                cancel,
            )
            .await
    }

    pub async fn backup(
        &self,
        id: Uuid,
        suffix: &str,
        permissions: FsPermissions,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if matches!(permissions, FsPermissions::PrivateReadOnly) {
            return Err(FileStorageError::InvalidPath(
                "privateReadOnly applies only to replacement",
            ));
        }
        #[cfg(unix)]
        let super::ReplacementMode::Native(mode) = permissions.mode();
        #[cfg(not(unix))]
        let mode = None;
        self.held_lease(id, None, cancel)
            .await?
            .backup_with(
                suffix,
                mode,
                matches!(permissions, FsPermissions::Preserve),
                cancel,
            )
            .await
    }

    pub async fn create_directory(
        &self,
        path: impl AsRef<Path>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let path = self.storage.resolve(path.as_ref())?;
        self.storage.create_directory(&path, cancel).await
    }

    pub async fn list(
        &self,
        path: impl AsRef<Path>,
        cancel: &CancellationToken,
    ) -> Result<Vec<DirectoryEntry>> {
        let path = self.storage.resolve(path.as_ref())?;
        self.storage.list(&path, cancel).await
    }

    pub async fn remove(&self, request: FsRemove, cancel: &CancellationToken) -> Result<()> {
        let path = match request {
            FsRemove::Path { path } => self.storage.resolve(Path::new(&path))?,
            FsRemove::Entry { parent, name } => {
                if Path::new(&name).components().next() != Some(Component::Normal(name.as_os_str()))
                {
                    return Err(FileStorageError::InvalidPath(
                        "expected a single directory entry name",
                    ));
                }
                let parent = self.storage.resolve(Path::new(&parent))?;
                self.storage.path(parent.root, parent.relative.join(name))?
            }
        };
        self.storage.remove(&path, cancel).await
    }

    /// Pins a file until close or the calling row's unload.
    pub async fn open_read(
        &self,
        caller: &Ctx,
        path: impl AsRef<Path>,
        cancel: &CancellationToken,
    ) -> Result<OpenRead> {
        let path = self.storage.resolve(path.as_ref())?;
        self.storage.open_reader(caller, &path, cancel).await
    }

    /// Reads the same pinned file, capped by the owner's existing 64 KiB limit.
    pub fn read_at<'a>(
        &'a self,
        read_id: String,
        offset: u64,
        limit: usize,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send + 'a {
        self.storage.read_at_id(read_id, offset, limit, cancel)
    }

    /// Releases the reader id; already accepted reads settle normally.
    pub fn close_read(&self, read_id: String) -> impl Future<Output = Result<()>> + Send + '_ {
        self.storage.close_reader(read_id)
    }
}

impl FileStorage {
    /// Opens a scoped storage path and binds the public reader ID to its calling row.
    pub async fn open_reader(
        self: &Arc<Self>,
        caller: &Ctx,
        path: &super::FilePath,
        cancel: &CancellationToken,
    ) -> Result<OpenRead> {
        let reader = self.open_read(path, cancel).await?;
        let id = reader.0.id;
        let size = reader.len();
        let owner = Arc::downgrade(self);
        let release = caller
            .effect(move || async move {
                if let Some(owner) = owner.upgrade() {
                    let entry = owner.state.lock().unwrap().readers.remove(&id);
                    drop(entry);
                }
                drop(reader);
                Ok(())
            })
            .map_err(|_| FileStorageError::Closed)?;
        let release = {
            let mut state = self.state.lock().unwrap();
            match state.readers.get_mut(&id) {
                Some(entry) => {
                    entry.release = Some(release);
                    None
                }
                None => Some(release),
            }
        };
        if let Some(release) = release {
            let _ = release.dispose().await;
            return Err(FileStorageError::Closed);
        }
        Ok(OpenRead {
            read_id: id.to_string(),
            size,
        })
    }

    async fn read_at_id(
        &self,
        read_id: String,
        offset: u64,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let id = Uuid::parse_str(&read_id)
            .map_err(|_| FileStorageError::InvalidPath("unknown read id"))?;
        let reader = {
            let state = self.state.lock().unwrap();
            self.check(cancel)?;
            state
                .readers
                .get(&id)
                .and_then(|entry| entry.reader.upgrade())
                .ok_or(FileStorageError::InvalidPath("unknown read id"))?
        };
        FileReader(reader).read_at(offset, limit, cancel).await
    }

    async fn close_reader(&self, read_id: String) -> Result<()> {
        let id = Uuid::parse_str(&read_id)
            .map_err(|_| FileStorageError::InvalidPath("unknown read id"))?;
        let release = {
            let mut state = self.state.lock().unwrap();
            let entry = state
                .readers
                .get_mut(&id)
                .ok_or(FileStorageError::InvalidPath("unknown read id"))?;
            let release = entry
                .release
                .take()
                .ok_or(FileStorageError::InvalidPath("unknown read id"))?;
            state.readers.remove(&id);
            release
        };
        release
            .dispose()
            .await
            .map_err(|_| FileStorageError::Closed)
    }
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "request", rename_all = "camelCase")]
enum Request {
    Resolve(crate::file_access::ResolvePath),
    Write(WorkspaceWrite),
    Edit(dim_agent_types::JsonObject),
    Glob(dim_agent_types::JsonObject),
    Grep(dim_agent_types::JsonObject),
    Mentions(MentionsRequest),
    Stat(PathRequest),
    Lock(PathRequest),
    Unlock(LeaseRequest),
    ReadBytes(ReadBytes),
    Replace(Replace),
    Backup(Backup),
    OpenRead(PathRequest),
    CreateDirectory(PathRequest),
    List(PathRequest),
    Remove(FsRemove),
    ReadAt(Read),
    CloseRead(Close),
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeaseRequest {
    lease_id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadBytes {
    path: String,
    limit: usize,
    lease_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase", deny_unknown_fields)]
enum Replace {
    Bytes {
        path: PathBuf,
        bytes: String,
        #[serde(default)]
        permissions: FsPermissions,
        lease_id: Option<Uuid>,
    },
    Source {
        path: PathBuf,
        source_path: PathBuf,
        limit: u64,
        #[serde(default)]
        permissions: FsPermissions,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Backup {
    lease_id: Uuid,
    suffix: String,
    #[serde(default)]
    permissions: FsPermissions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathRequest {
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Read {
    read_id: String,
    offset: u64,
    limit: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Close {
    read_id: String,
}

impl HostDispatch for FsHost {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"resolve":"async","write":"async","edit":"async","glob":"async","grep":"async","mentions":"async","stat":"async","replace":"async","backup":"async","lock":"async", "unlock":"async", "readBytes":"async", "openRead":"async", "readAt":"async", "closeRead":"async", "createDirectory":"async", "list":"async", "remove":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let [request]: [serde_json::Value; 1] = serde_json::from_value(args.json()?)
            .map_err(|error| Error::value(error.to_string()))?;
        if !request.is_object() {
            return Err(Error::value("fsHost request must be an object"));
        }
        let request: Request =
            serde_json::from_value(serde_json::json!({"method":method,"request":request}))
                .map_err(|error| Error::value(error.to_string()))?;
        let caller = rutis_bridge::session::caller_row().map(|row| row.ctx().clone());
        let owner = self.clone();
        Ok(Value::cancellable_future(async move {
            let cancel = CancellationToken::new();
            let _dropped = cancel.clone().drop_guard();
            let reply = match request {
                Request::Resolve(request) => {
                    let home = std::env::home_dir()
                        .ok_or_else(|| failure(FileStorageError::Unsupported))?;
                    let resolved = owner
                        .resolve_workspace_path(request, &home, &owner.system_roots, &cancel)
                        .await
                        .map_err(|error| {
                            rutis_bridge::exports::failure(
                                "FileStorageError",
                                match &error {
                                    crate::host::HostError::Cancelled(_) => "cancelled",
                                    _ => "invalid",
                                },
                                error.to_string(),
                                None,
                            )
                        })?;
                    serde_json::json!(resolved)
                }
                request @ (Request::Write(_) | Request::Edit(_)) => {
                    let mutation = match request {
                        Request::Write(input) => WorkspaceMutation::Write(input),
                        Request::Edit(input) => WorkspaceMutation::Edit(input),
                        _ => unreachable!(),
                    };
                    let result = owner
                        .mutate_workspace_file(mutation, &cancel)
                        .await
                        .map_err(|error| {
                            let code = match &error {
                                crate::host::HostError::Cancelled(_) => "cancelled",
                                _ => "io",
                            };
                            rutis_bridge::exports::failure(
                                "FileStorageError",
                                code,
                                error.to_string(),
                                None,
                            )
                        })?;
                    serde_json::json!(result)
                }
                request @ (Request::Glob(_) | Request::Grep(_)) => {
                    let (kind, input) = match request {
                        Request::Glob(input) => (SearchKind::Glob, input),
                        Request::Grep(input) => (SearchKind::Grep, input),
                        _ => unreachable!(),
                    };
                    let path = input
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(Path::new)
                        .filter(|path| path.is_absolute())
                        .ok_or_else(|| {
                            failure(FileStorageError::InvalidPath(
                                "expected an absolute search root",
                            ))
                        })?;
                    let home = std::env::home_dir()
                        .ok_or_else(|| failure(FileStorageError::Unsupported))?;
                    let result = owner
                        .workspace_search(kind, path, &home, None, &input, &cancel)
                        .await
                        .map_err(|error| {
                            let code = match &error {
                                crate::host::HostError::Cancelled(_) => "cancelled",
                                _ => "io",
                            };
                            rutis_bridge::exports::failure(
                                "FileStorageError",
                                code,
                                error.to_string(),
                                None,
                            )
                        })?;
                    serde_json::json!({"result":result})
                }
                Request::Mentions(request) => {
                    serde_json::json!(owner.mentions(request, &cancel).await.map_err(failure)?)
                }
                Request::Stat(request) => serde_json::json!(
                    owner
                        .stat(Path::new(&request.path), &cancel)
                        .await
                        .map_err(failure)?
                ),
                Request::Lock(request) => {
                    let caller =
                        caller.ok_or_else(|| Error::value("fsHost.lock requires a calling row"))?;
                    let id = owner
                        .lock(Some(&caller), &request.path, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::json!({"leaseId":id})
                }
                Request::Unlock(request) => {
                    owner.unlock(request.lease_id).await.map_err(failure)?;
                    serde_json::json!({})
                }
                Request::Replace(Replace::Bytes {
                    path,
                    bytes,
                    permissions,
                    lease_id,
                }) => {
                    let bytes = STANDARD.decode(bytes).map_err(|_| {
                        failure(FileStorageError::InvalidPath("invalid base64 bytes"))
                    })?;
                    owner
                        .replace(&path, bytes, permissions, lease_id, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::json!({})
                }
                Request::Replace(Replace::Source {
                    path,
                    source_path,
                    limit,
                    permissions,
                }) => {
                    let size = owner
                        .replace_from_path(&path, source_path, limit, permissions, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::json!({"size": size})
                }
                Request::Backup(request) => {
                    owner
                        .backup(
                            request.lease_id,
                            &request.suffix,
                            request.permissions,
                            &cancel,
                        )
                        .await
                        .map_err(failure)?;
                    serde_json::json!({})
                }
                Request::ReadBytes(request) => {
                    let bytes = owner
                        .read_bytes(
                            Path::new(&request.path),
                            request.limit,
                            request.lease_id,
                            &cancel,
                        )
                        .await
                        .map_err(failure)?;
                    serde_json::json!({"size":bytes.len(),"bytes":STANDARD.encode(bytes)})
                }
                Request::CreateDirectory(request) => {
                    owner
                        .create_directory(&request.path, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::json!({})
                }
                Request::List(request) => {
                    let entries = owner.list(&request.path, &cancel).await.map_err(failure)?;
                    serde_json::json!({"entries": entries})
                }
                Request::Remove(request) => {
                    owner.remove(request, &cancel).await.map_err(failure)?;
                    serde_json::json!({})
                }
                Request::OpenRead(request) => {
                    let caller = caller
                        .ok_or_else(|| Error::value("fsHost.openRead requires a calling row"))?;
                    let result = owner
                        .open_read(&caller, request.path, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::to_value(result).map_err(|error| Error::value(error.to_string()))?
                }
                Request::ReadAt(request) => {
                    let bytes = owner
                        .read_at(request.read_id, request.offset, request.limit, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::json!({"bytes":STANDARD.encode(bytes)})
                }
                Request::CloseRead(request) => {
                    owner.close_read(request.read_id).await.map_err(failure)?;
                    serde_json::json!({})
                }
            };
            Ok(reply.into())
        }))
    }
}

fn failure(error: FileStorageError) -> Error {
    let code = match &error {
        FileStorageError::InvalidPath(_) => "invalid",
        FileStorageError::Closed | FileStorageError::Unsupported => "node_unavailable",
        FileStorageError::Cancelled => "cancelled",
        FileStorageError::Locked => "locked",
        FileStorageError::Io(error)
            if error
                .get_ref()
                .is_some_and(|cause| cause.is::<super::ReadLimitExceeded>()) =>
        {
            "too_large"
        }
        FileStorageError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => "not_found",
        _ => "io",
    };
    rutis_bridge::exports::failure("FileStorageError", code, error.to_string(), None)
}
