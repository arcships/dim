use super::{FileReader, FileStorage, FileStorageError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis::Ctx;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRead {
    pub read_id: String,
    pub size: u64,
}

/// The pinned-reader operations on the existing filesystem owner.
#[derive(Clone)]
pub struct FsHost(Arc<FileStorage>);

impl FsHost {
    pub(super) fn new(owner: Arc<FileStorage>) -> Self {
        Self(owner)
    }

    /// Pins a file until close or the calling row's unload.
    pub fn open_read<'a>(
        &'a self,
        caller: &'a Ctx,
        path: String,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<OpenRead>> + Send + 'a {
        self.0.open_reader(caller, path, cancel)
    }

    /// Reads the same pinned file, capped by the owner's existing 64 KiB limit.
    pub fn read_at<'a>(
        &'a self,
        read_id: String,
        offset: u64,
        limit: usize,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send + 'a {
        self.0.read_at_id(read_id, offset, limit, cancel)
    }

    /// Releases the reader id; already accepted reads settle normally.
    pub fn close_read(&self, read_id: String) -> impl Future<Output = Result<()>> + Send + '_ {
        self.0.close_reader(read_id)
    }
}

impl FileStorage {
    async fn open_reader(
        self: &Arc<Self>,
        caller: &Ctx,
        path: String,
        cancel: &CancellationToken,
    ) -> Result<OpenRead> {
        let path = self.resolve(Path::new(&path))?;
        let reader = self.open_read(&path, cancel).await?;
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
    OpenRead(Open),
    ReadAt(Read),
    CloseRead(Close),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
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
        Some(serde_json::json!({"openRead":"async", "readAt":"async", "closeRead":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let [request]: [serde_json::Value; 1] = serde_json::from_value(args.json()?)
            .map_err(|error| Error::Value(error.to_string()))?;
        if !request.is_object() {
            return Err(Error::Value("fsHost request must be an object".into()));
        }
        let request: Request =
            serde_json::from_value(serde_json::json!({"method":method,"request":request}))
                .map_err(|error| Error::Value(error.to_string()))?;
        let caller = rutis_bridge::session::caller_row().map(|row| row.ctx().clone());
        let owner = self.clone();
        Ok(Value::cancellable_future(async move {
            let cancel = CancellationToken::new();
            let _dropped = cancel.clone().drop_guard();
            let reply = match request {
                Request::OpenRead(request) => {
                    let caller = caller.ok_or_else(|| {
                        Error::Value("fsHost.openRead requires a calling row".into())
                    })?;
                    let result = owner
                        .open_read(&caller, request.path, &cancel)
                        .await
                        .map_err(failure)?;
                    serde_json::to_value(result).map_err(|error| Error::Value(error.to_string()))?
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
        FileStorageError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => "not_found",
        _ => "io",
    };
    rutis_bridge::exports::failure("FileStorageError", code, error.to_string(), None)
}
