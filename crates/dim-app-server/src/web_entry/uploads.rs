use super::*;
use axum::extract::{Multipart, Path, Query};
use dim_agent_capabilities::{host::SessionFilesHost, methods::SessionMethods};
use serde_json::json;
use tokio::io::AsyncWriteExt;

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
    let directory = tempfile::tempdir().map_err(|e| RpcError::invalid(e.to_string()))?;
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
    let files = match site.hub.with_rpc(|rpc| rpc.ctx.get::<SessionFilesHost>()) {
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
