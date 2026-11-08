use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_capabilities::{
    file_storage::{FileReader, FileStorage, FileStorageError},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
};
use dim_agent_runtime::{Names, PublicService, SessionIdentity, SessionLifecycle, SessionPhase};
use rutis::{
    Ctx,
    host::{BoxFuture, CtxHost, DynPlugin, Effect, InstanceId, Key, TypeKey},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};

#[derive(Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct ResourceError {
    pub code: String,
    pub message: String,
}
impl ResourceError {
    fn service(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub(crate) fn invalid(message: &str) -> Self {
        Self::service("invalid_params", message)
    }
}

#[derive(Default)]
pub(super) struct Reads {
    gate: Mutex<()>,
    pub stop: CancellationToken,
    tasks: TaskTracker,
}
impl Reads {
    pub fn new(stop: CancellationToken) -> Self {
        Self {
            stop,
            ..Self::default()
        }
    }
    pub fn enter(&self) -> Result<TaskTrackerToken, ResourceError> {
        let _gate = self.gate.lock().unwrap();
        if self.stop.is_cancelled() {
            return Err(gone());
        }
        Ok(self.tasks.token())
    }
    pub async fn close(&self) {
        {
            let _gate = self.gate.lock().unwrap();
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}

pub(crate) struct Read {
    pub file: FileReader,
    pub stop: CancellationToken,
    pub token: TaskTrackerToken,
    pub media_type: String,
    pub attachment: bool,
    pub etag: Option<String>,
}

/// The page a read returns when the caller names no `limit`.
pub(crate) const DEFAULT_PAGE: usize = 1024 * 1024;
/// The most a caller may read in one reply; a larger `limit` is clamped.
pub(crate) const MAX_PAGE: usize = 4 * 1024 * 1024;

/// One page of a UI resource: the reply both read methods share. `offset` is
/// where the page starts and `data` is `bytes` bytes of the resource from
/// there, base64 encoded, so the next page starts at `offset + bytes`. The
/// resource is immutable and `total` is fixed: a client is done when
/// `offset + bytes == total` and never needs an empty page to learn the end.
/// A page can be empty only when the caller asks for `offset == total`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    pub offset: u64,
    pub bytes: usize,
    pub total: u64,
    pub media_type: String,
    pub data: String,
}

/// Reads one page of `read`, honoring the caller's cancellation. The read
/// keeps its [`Reads`] token for the whole page, so a withdrawal waits for it
/// and a closed resource answers `resource_gone` instead of a partial page.
/// `offset` defaults to 0 and `limit` to [`DEFAULT_PAGE`], clamped to
/// [`MAX_PAGE`].
pub(crate) async fn read_page(
    read: Read,
    offset: Option<u64>,
    limit: Option<usize>,
    cancel: CancellationToken,
) -> Result<Page, ResourceError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE).min(MAX_PAGE);
    if limit == 0 {
        return Err(ResourceError::invalid("limit must be positive"));
    }
    let total = read.file.len();
    let offset = offset.unwrap_or(0);
    if offset > total {
        return Err(ResourceError::invalid("offset is beyond the resource"));
    }
    let end = offset.saturating_add(limit as u64).min(total);
    let mut data = Vec::with_capacity((end - offset) as usize);
    let stop = read.stop.child_token();
    while offset + (data.len() as u64) < end {
        let at = offset + data.len() as u64;
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(gone()),
            _ = stop.cancelled() => return Err(gone()),
            chunk = read.file.read_at(at, (end - at) as usize, &stop) => chunk,
        }
        .map_err(page_error)?;
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
    }
    Ok(Page {
        offset,
        bytes: data.len(),
        total,
        media_type: read.media_type.clone(),
        data: STANDARD.encode(&data),
    })
}

fn page_error(error: FileStorageError) -> ResourceError {
    match error {
        FileStorageError::Cancelled | FileStorageError::Closed => gone(),
        other => file_error(other),
    }
}

impl ResourceError {
    /// The wire error a client receives: the resource code becomes the
    /// application error of the same name.
    pub(crate) fn wire(self) -> RpcError {
        RpcError::service(&self.code, self.message)
    }
}

/// The `session.ui.resources.read` request. Rpc routes the call on
/// `sessionId`, which the handler does not need, but the request declares it
/// so a mistyped member is `invalid_params` rather than silently ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourcePageRequest {
    #[serde(rename = "sessionId")]
    _session_id: String,
    id: String,
    offset: Option<u64>,
    limit: Option<usize>,
}

struct Published {
    owner: InstanceId,
    file: Mutex<Option<FileReader>>,
    reads: Reads,
    media_type: String,
    attachment: bool,
}
impl Published {
    async fn close(&self) {
        self.reads.close().await;
        self.file.lock().unwrap().take();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRef {
    pub session_id: String,
    pub id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishFile {
    /// Absolute host path inside an already authorized FileStorage root.
    pub path: String,
    pub media_type: String,
}

struct Registration {
    resource: Arc<Published>,
    cleanup: rutis::Disposer,
}

pub struct UiResources {
    session_id: String,
    lifecycle: Arc<SessionLifecycle>,
    files: Arc<FileStorage>,
    resources: Arc<Mutex<HashMap<String, Registration>>>,
    reads: Arc<Reads>,
}
impl PublicService for UiResources {}

#[rutis::service]
impl UiResources {
    /// Publishes a finalized regular file at a canonical absolute host path
    /// inside an already authorized FileStorage root; relative paths are invalid.
    /// The calling part owns the reference until withdrawal or unload.
    pub async fn publish_file(
        &self,
        caller: &Ctx,
        request: PublishFile,
    ) -> Result<ResourceRef, ResourceError> {
        if !Path::new(&request.path).is_absolute() {
            return Err(ResourceError::invalid(
                "Expected an absolute host file path",
            ));
        }
        let _operation = self.reads.enter()?;
        if self.lifecycle.phase() >= SessionPhase::Closing {
            return Err(gone());
        }
        let path = self
            .files
            .resolve(Path::new(&request.path))
            .map_err(file_error)?;
        let cancel = caller.cancellation_token().child_token();
        let _cancel = cancel.clone().drop_guard();
        let file = tokio::select! {
            _ = self.reads.stop.cancelled() => return Err(gone()),
            _ = self.lifecycle.closing() => return Err(gone()),
            file = self.files.open_read(&path, &cancel) => file.map_err(file_error)?,
        };
        let inline = matches!(
            request.media_type.as_str(),
            "image/png"
                | "image/jpeg"
                | "image/gif"
                | "image/webp"
                | "image/avif"
                | "audio/mpeg"
                | "audio/ogg"
                | "audio/wav"
                | "audio/mp4"
                | "audio/webm"
                | "video/mp4"
                | "video/webm"
                | "video/ogg"
                | "text/plain"
                | "application/pdf"
        );
        let published = Arc::new(Published {
            owner: caller.instance(),
            file: Mutex::new(Some(file)),
            reads: Reads::new(self.reads.stop.child_token()),
            media_type: if inline {
                request.media_type
            } else {
                "application/octet-stream".into()
            },
            attachment: !inline,
        });
        let id = uuid::Uuid::new_v4().to_string();
        let resources = self.resources.clone();
        let cleanup = published.clone();
        let key = id.clone();
        let mut entries = self.resources.lock().unwrap();
        if self.reads.stop.is_cancelled()
            || self.lifecycle.phase() >= SessionPhase::Closing
            || caller.cancellation_token().is_cancelled()
        {
            return Err(gone());
        }
        let disposer = caller
            .effect(move || async move {
                cleanup.close().await;
                resources.lock().unwrap().remove(&key);
                Ok(())
            })
            .map_err(|error| ResourceError::service("resource_closed", error.to_string()))?;
        entries.insert(
            id.clone(),
            Registration {
                resource: published,
                cleanup: disposer,
            },
        );
        Ok(ResourceRef {
            session_id: self.session_id.clone(),
            id,
        })
    }

    /// Withdraws only a reference this calling part published; waits for reads to end.
    pub async fn withdraw(
        &self,
        caller: &Ctx,
        reference: ResourceRef,
    ) -> Result<(), ResourceError> {
        if reference.session_id != self.session_id {
            return Err(gone());
        }
        let resource = {
            let entries = self.resources.lock().unwrap();
            match entries.get(&reference.id) {
                Some(entry) if entry.resource.owner != caller.instance() => {
                    return Err(ResourceError::invalid("Resource belongs to another part"));
                }
                Some(entry) => Some(entry.resource.clone()),
                None => None,
            }
        };
        if let Some(resource) = resource {
            resource.close().await;
            let entry = self.resources.lock().unwrap().remove(&reference.id);
            if let Some(entry) = entry {
                entry.cleanup.dispose().await.map_err(file_error)?;
            }
        }
        Ok(())
    }
}
impl UiResources {
    pub(crate) fn read(&self, id: &str) -> Result<Read, ResourceError> {
        if self.lifecycle.phase() != SessionPhase::Active {
            return Err(gone());
        }
        let entries = self.resources.lock().unwrap();
        let entry = &entries.get(id).ok_or_else(gone)?.resource;
        let token = entry.reads.enter()?;
        let file = entry.file.lock().unwrap().clone().ok_or_else(gone)?;
        Ok(Read {
            file,
            stop: entry.reads.stop.clone(),
            token,
            media_type: entry.media_type.clone(),
            attachment: entry.attachment,
            etag: None,
        })
    }
    async fn close(&self) {
        self.reads.close().await;
        let entries = std::mem::take(&mut *self.resources.lock().unwrap());
        for entry in entries.into_values() {
            if let Err(error) = entry.cleanup.dispose().await {
                tracing::warn!(%error, "UI resource cleanup failed");
            }
        }
    }
}

pub(super) fn gone() -> ResourceError {
    ResourceError::service("resource_gone", "UI resource is unavailable")
}
pub(super) fn file_error(error: impl std::fmt::Display) -> ResourceError {
    ResourceError::service("resource_unavailable", error.to_string())
}

pub(super) struct UiResourcesPlugin {
    identity: Key<SessionIdentity>,
    lifecycle: Key<SessionLifecycle>,
    files: Key<FileStorage>,
    methods: Key<SessionMethods>,
    key: Key<UiResources>,
}
impl UiResourcesPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
            lifecycle: names.key()?,
            files: names.key()?,
            methods: names.key()?,
            key: names.key()?,
        })
    }
}
impl DynPlugin for UiResourcesPlugin {
    fn name(&self) -> &str {
        "UiResources"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.identity.erase(),
            self.lifecycle.erase(),
            self.files.erase(),
            self.methods.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let service = Arc::new(UiResources {
                session_id: ctx.require_at(&self.identity)?.session_id.clone(),
                lifecycle: lifecycle.clone(),
                files: ctx.require_at(&self.files)?,
                resources: Arc::default(),
                reads: Arc::new(Reads::new(ctx.cancellation_token())),
            });
            let cleanup = service.clone();
            let stop = ctx.cancellation_token();
            let running = service.clone();
            let task = tokio::spawn(async move {
                tokio::select! {
                    _ = stop.cancelled() => {},
                    _ = async { if lifecycle.activated().await { lifecycle.closing().await; } } => {},
                }
                running.close().await;
            });
            ctx.effect(move || async move {
                cleanup.close().await;
                let _ = task.await;
                Ok(())
            })?;
            let reader = service.clone();
            ctx.require_at(&self.methods)?.register(
                ctx,
                "session.ui.resources",
                vec![MethodDecl::new(
                    "session.ui.resources.read",
                    false,
                    Interruption::Report,
                )],
                Arc::new(move |call, stop, _progress| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let reader = reader.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "session.ui.resources.read" => {
                                let request: ResourcePageRequest = params(value)?;
                                let read = reader.read(&request.id).map_err(ResourceError::wire)?;
                                reply(
                                    read_page(read, request.offset, request.limit, stop)
                                        .await
                                        .map_err(ResourceError::wire)?,
                                )
                            }
                            _ => unreachable!("registered session UI resource method"),
                        }
                    })
                }),
            )?;
            ctx.provide_at(self.key.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}
