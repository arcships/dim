use super::*;
use axum::extract::{Multipart, Path, Query};
use dim_agent_capabilities::{host::SessionFiles, methods::SessionMethods};
use serde_json::json;
use tokio::io::AsyncWriteExt;

pub(super) struct Staging {
    path: PathBuf,
    _lock: std::fs::File,
    _temporary: Option<tempfile::TempDir>,
}
impl Staging {
    pub(super) fn open(database: Option<PathBuf>) -> Result<Self, RpcError> {
        let temporary = if database.is_none() {
            Some(tempfile::tempdir().map_err(|e| RpcError::invalid(e.to_string()))?)
        } else {
            None
        };
        let path = match database {
            Some(database) => {
                let mut path = database.into_os_string();
                path.push(".web-uploads");
                PathBuf::from(path)
            }
            None => temporary
                .as_ref()
                .expect("temporary staging")
                .path()
                .to_owned(),
        };
        let open = || -> std::io::Result<std::fs::File> {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if !std::fs::symlink_metadata(&path)?.is_dir() {
                return Err(std::io::Error::other("Upload staging is not a directory"));
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let lock = options.open(path.join("lock"))?;
            lock.try_lock().map_err(std::io::Error::other)?;
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("upload-") {
                    if entry.file_type()?.is_dir() {
                        std::fs::remove_dir_all(entry.path())?;
                    } else {
                        std::fs::remove_file(entry.path())?;
                    }
                }
            }
            Ok(lock)
        };
        let lock = open().map_err(|e| RpcError::invalid(e.to_string()))?;
        Ok(Self {
            path,
            _lock: lock,
            _temporary: temporary,
        })
    }
}

#[derive(serde::Deserialize)]
pub(super) struct UploadOptions {
    #[serde(default)]
    image: bool,
}

pub(super) async fn upload(
    State(site): State<Arc<Site>>,
    Path(session): Path<String>,
    headers: HeaderMap,
    Query(options): Query<UploadOptions>,
    mut multipart: Multipart,
) -> Response {
    if !site.authenticated(&headers) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web authentication required",
        );
    }
    if !site.origin_allowed(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "Origin is not allowed",
        );
    }
    let Ok(_permit) = site.uploads.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "uploads_busy",
            "Upload concurrency limit reached",
        );
    };
    let closing = site.stopping.clone();
    let received = tokio::select! {
        _ = closing.cancelled() => Err(RpcError::interrupted()),
        result = tokio::time::timeout(Duration::from_secs(120), receive(site.clone(), session, options.image, &mut multipart)) => {
            result.unwrap_or_else(|_| Err(RpcError::invalid("Upload timed out")))
        }
    };
    match received {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(failure) => (StatusCode::BAD_REQUEST, axum::Json(failure)).into_response(),
    }
}

async fn receive(
    site: Arc<Site>,
    session: String,
    image: bool,
    multipart: &mut Multipart,
) -> Result<serde_json::Value, RpcError> {
    let index = site.hub.with_rpc(|rpc| rpc.index.clone())?;
    let usage = index.acquire(&session).await?;
    let directory = tempfile::Builder::new()
        .prefix("upload-")
        .tempdir_in(&site.staging.path)
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    let path = directory.path().join("upload");
    let field = multipart
        .next_field()
        .await
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    let mut field = field.ok_or_else(|| RpcError::invalid("One file is required"))?;
    if field.name() != Some("file") {
        return Err(RpcError::invalid("Expected the file field"));
    }
    let mut target = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .await
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    let mut length = 0usize;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), field.chunk())
        .await
        .map_err(|_| RpcError::invalid("Upload timed out"))?
        .map_err(|e| RpcError::invalid(e.to_string()))?
    {
        length += chunk.len();
        if length > 16 * 1024 * 1024 {
            return Err(RpcError::invalid("Upload exceeds 16 MiB"));
        }
        target
            .write_all(&chunk)
            .await
            .map_err(|e| RpcError::invalid(e.to_string()))?;
    }
    target
        .flush()
        .await
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    drop(target);
    drop(field);
    if multipart
        .next_field()
        .await
        .map_err(|e| RpcError::invalid(e.to_string()))?
        .is_some()
    {
        return Err(RpcError::invalid("Only one file is accepted"));
    }
    let methods = usage
        .session
        .session_service::<SessionMethods>()
        .map_err(RpcError::from)?;
    let provider = methods
        .provider("attachment.import")
        .ok_or_else(RpcError::unknown_method)?;
    let cancel = site.stopping.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let accepted = provider.accept(
        "attachment.import".into(),
        json!({"sessionId": session, "path": path, "image": image}),
        None,
        cancel,
        |_| {},
    )?;
    let result = accepted.reply.await.map_err(|_| RpcError::interrupted())?;
    let _ = accepted.settled.await;
    result
}

pub(super) async fn download(
    State(site): State<Arc<Site>>,
    Path((session, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !site.authenticated(&headers) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web authentication required",
        );
    }
    if headers.contains_key(header::ORIGIN) && !site.origin_allowed(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "Origin is not allowed",
        );
    }
    let files = match site.hub.with_rpc(|rpc| rpc.ctx.get::<SessionFiles>()) {
        Ok(Some(files)) => files,
        _ => {
            return error(
                StatusCode::NOT_FOUND,
                "file_missing",
                "Session file is unavailable",
            );
        }
    };
    let cancel = site.stopping.child_token();
    let first = match files
        .0
        .read(session.clone(), file.clone(), 0, cancel.clone())
        .await
    {
        Ok(first) => first,
        Err(_) => {
            return error(
                StatusCode::NOT_FOUND,
                "file_missing",
                "Session file is unavailable",
            );
        }
    };
    let media = match dim_agent_capabilities::images::sniff(&first) {
        Some(dim_agent_capabilities::images::Sniffed::Image(kind)) => kind.media_type(),
        _ => "application/octet-stream",
    };
    let stream = futures_util::stream::try_unfold(
        (files, session, file, cancel, 0u64, Some(first)),
        |(files, session, file, cancel, offset, first)| async move {
            let data = match first {
                Some(data) => data,
                None => files
                    .0
                    .read(session.clone(), file.clone(), offset, cancel.clone())
                    .await
                    .map_err(std::io::Error::other)?,
            };
            if data.is_empty() {
                return Ok::<_, std::io::Error>(None);
            }
            let next = offset + data.len() as u64;
            Ok(Some((
                Bytes::from(data),
                (files, session, file, cancel, next, None),
            )))
        },
    );
    (
        [
            (header::CONTENT_TYPE, media),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn management(
    site: &Site,
    method: &str,
    params: serde_json::Value,
    cancel: CancellationToken,
) -> Result<serde_json::Value, RpcError> {
    let table = site.hub.with_rpc(|rpc| rpc.management.clone())?;
    let provider = table
        .provider(method)
        .ok_or_else(RpcError::unknown_method)?;
    let accepted = provider.accept(method.into(), params, None, cancel, |_| {})?;
    let result = accepted.reply.await.map_err(|_| RpcError::interrupted())?;
    let _ = accepted.settled.await;
    result
}

pub(super) async fn modality(
    State(site): State<Arc<Site>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if !site.authenticated(&headers) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web authentication required",
        );
    }
    if !site.origin_allowed(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "Origin is not allowed",
        );
    }
    let Ok(_permit) = site.uploads.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "uploads_busy",
            "Upload concurrency limit reached",
        );
    };
    let cancel = site.stopping.child_token();
    let _guard = cancel.clone().drop_guard();
    let receive = async {
        let directory = tempfile::Builder::new()
            .prefix("upload-")
            .tempdir_in(&site.staging.path)
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        let field = multipart
            .next_field()
            .await
            .map_err(|e| RpcError::invalid(e.to_string()))?
            .ok_or_else(|| RpcError::invalid("A request field is required"))?;
        if field.name() != Some("request") {
            return Err(RpcError::invalid("The first field must be request"));
        }
        let mut request_bytes = Vec::new();
        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| RpcError::invalid(e.to_string()))?
        {
            if request_bytes.len() + chunk.len() > 64 * 1024 {
                return Err(RpcError::invalid("Task request exceeds 64 KiB"));
            }
            request_bytes.extend_from_slice(&chunk);
        }
        drop(field);
        let mut request: serde_json::Value =
            serde_json::from_slice(&request_bytes).map_err(|e| RpcError::invalid(e.to_string()))?;
        let params = request
            .as_object_mut()
            .ok_or_else(|| RpcError::invalid("Task request must be an object"))?;
        if params.contains_key("files") {
            return Err(RpcError::invalid("Supply files as multipart fields"));
        }
        let mut files = Vec::new();
        let mut bytes = 0usize;
        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|e| RpcError::invalid(e.to_string()))?
        {
            if field.name() != Some("file") || files.len() >= 20 {
                return Err(RpcError::invalid("Supply at most 20 file fields"));
            }
            let name = field
                .file_name()
                .ok_or_else(|| RpcError::invalid("A file name is required"))?
                .to_owned();
            let media = field
                .content_type()
                .ok_or_else(|| RpcError::invalid("A file media type is required"))?
                .to_owned();
            let path = directory.path().join(files.len().to_string());
            let mut target = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .await
                .map_err(|e| RpcError::invalid(e.to_string()))?;
            while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), field.chunk())
                .await
                .map_err(|_| RpcError::invalid("Upload timed out"))?
                .map_err(|e| RpcError::invalid(e.to_string()))?
            {
                bytes += chunk.len();
                if bytes > 100 * 1024 * 1024 {
                    return Err(RpcError::invalid("Task inputs exceed 100 MiB"));
                }
                target
                    .write_all(&chunk)
                    .await
                    .map_err(|e| RpcError::invalid(e.to_string()))?;
            }
            target
                .flush()
                .await
                .map_err(|e| RpcError::invalid(e.to_string()))?;
            files.push(json!({"name":name,"media_type":media,"node_path":path}));
        }
        params.insert("files".into(), files.into());
        management(&site, "modality.task.submit", request, cancel.clone()).await
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(RpcError::interrupted()),
        result = tokio::time::timeout(Duration::from_secs(120), receive) => result.unwrap_or_else(|_| Err(RpcError::invalid("Upload timed out"))),
    };
    match result {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, axum::Json(error)).into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskResultInput {
    task_id: String,
    name: String,
}

pub(super) async fn attach_result(
    State(site): State<Arc<Site>>,
    Path(session): Path<String>,
    headers: HeaderMap,
    axum::Json(input): axum::Json<TaskResultInput>,
) -> Response {
    if !site.authenticated(&headers) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web authentication required",
        );
    }
    if !site.origin_allowed(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "Origin is not allowed",
        );
    }
    let Ok(_permit) = site.uploads.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "uploads_busy",
            "Upload concurrency limit reached",
        );
    };
    let cancel = site.stopping.child_token();
    let _guard = cancel.clone().drop_guard();
    let result = async {
        let source = management(
            &site,
            "modality.task.export",
            json!({"task_id":input.task_id,"name":input.name}),
            cancel.clone(),
        )
        .await?;
        let source: dim_agent_types::TaskFileSource =
            serde_json::from_value(source).map_err(|error| RpcError::invalid(error.to_string()))?;
        let index = site.hub.with_rpc(|rpc| rpc.index.clone())?;
        let usage = index.acquire(&session).await?;
        let methods = usage
            .session
            .session_service::<SessionMethods>()
            .map_err(RpcError::from)?;
        let provider = methods
            .provider("attachment.import")
            .ok_or_else(RpcError::unknown_method)?;
        let accepted = provider.accept(
            "attachment.import".into(),
            json!({"sessionId":session,"path":source.path,"image":source.image}),
            None,
            cancel.clone(),
            |_| {},
        )?;
        let result = accepted.reply.await.map_err(|_| RpcError::interrupted())?;
        let _ = accepted.settled.await;
        result
    }
    .await;
    match result {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, axum::Json(error)).into_response(),
    }
}
