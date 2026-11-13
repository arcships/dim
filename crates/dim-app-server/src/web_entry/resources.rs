use super::*;
use crate::ui_extensions::{Read, UiExtensions, UiResources};
use axum::extract::Path;
use futures_util::stream;

fn denial(site: &Site, headers: &HeaderMap) -> Option<Response> {
    if headers.contains_key(header::ORIGIN) && !site.origin_allowed(headers)
        || headers
            .get("sec-fetch-site")
            .is_some_and(|value| value == "cross-site")
    {
        return Some(error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "Cross-site resource request",
        ));
    }
    if !site.authenticated(headers) {
        return Some(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web authentication required",
        ));
    }
    None
}

pub(super) async fn extension(
    State(site): State<Arc<Site>>,
    Path((generation, path)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = denial(&site, &headers) {
        return response;
    }
    let owner = match site.hub.with_rpc(|rpc| rpc.ctx.get::<UiExtensions>()) {
        Ok(Some(owner)) => owner,
        _ => return missing(),
    };
    match owner.read(&generation, &path).await {
        Ok(read) => response(read, method, headers, ()),
        Err(_) => missing(),
    }
}

pub(super) async fn session_resource(
    State(site): State<Arc<Site>>,
    Path((session_id, id)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = denial(&site, &headers) {
        return response;
    }
    let index = match site.hub.with_rpc(|rpc| rpc.index.clone()) {
        Ok(index) => index,
        Err(_) => return missing(),
    };
    if index
        .version_of(&session_id)
        .is_none_or(|recorded| recorded > crate::protocol::PROTOCOL_VERSION)
        || index.unavailable_session(&session_id).is_some()
    {
        return missing();
    }
    let usage = match index.acquire(&session_id).await {
        Ok(usage) => usage,
        Err(_) => return missing(),
    };
    let owner = match usage.session.session_service::<UiResources>() {
        Ok(owner) => owner,
        Err(_) => return missing(),
    };
    match owner.read(&id) {
        Ok(read) => response(read, method, headers, usage),
        Err(_) => missing(),
    }
}

fn missing() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "resource_gone",
        "UI resource is unavailable",
    )
}

fn response<H: Send + 'static>(
    read: Read,
    method: Method,
    headers: HeaderMap,
    hold: H,
) -> Response {
    let total = read.file.len();
    let mut response = Response::new(Body::empty());
    let target = response.headers_mut();
    target.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&read.media_type).expect("fixed MIME"),
    );
    target.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if read.etag.is_some() {
            "private, no-cache"
        } else {
            "private, no-store"
        }),
    );
    target.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if read.attachment {
        target.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    if let Some(etag) = &read.etag {
        target.insert(
            header::ETAG,
            HeaderValue::from_str(etag).expect("generated ETag"),
        );
        if headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|value| value.trim() == etag || value.trim() == "*")
            })
        {
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            return response;
        }
    }
    let range = if method == Method::GET
        && headers.get(header::IF_RANGE).is_none_or(|value| {
            read.etag
                .as_ref()
                .is_some_and(|etag| value == etag.as_str())
        }) {
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(|value| range(value, total))
            .transpose()
    } else {
        Ok(None)
    };
    let (offset, length) = match range {
        Ok(Some(Some((offset, length)))) => {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {offset}-{}/{total}", offset + length - 1))
                    .unwrap(),
            );
            (offset, length)
        }
        Ok(_) => (0, total),
        Err(()) => {
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
            );
            return response;
        }
    };
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if method == Method::HEAD || length == 0 {
        return response;
    }
    let stop = read.stop.child_token();
    let guard = stop.clone().drop_guard();
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        let (_token, _hold) = (read.token, hold);
        let end = offset + length;
        let mut offset = offset;
        while offset < end {
            let result = read
                .file
                .read_at(offset, (end - offset).min(65536) as usize, &stop)
                .await;
            let failed = result.is_err();
            let result = result
                .map(|bytes| {
                    offset += bytes.len() as u64;
                    Bytes::from(bytes)
                })
                .map_err(std::io::Error::other);
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                sent = sender.send(result) => if sent.is_err() || failed { break; },
            }
        }
        drop(read.file);
    });
    *response.body_mut() = Body::from_stream(stream::unfold(
        (receiver, guard),
        |(mut receiver, guard)| async move {
            receiver.recv().await.map(|item| (item, (receiver, guard)))
        },
    ));
    response
}

fn range(value: &str, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if value.contains(',') {
        return Ok(None);
    }
    let (start, end) = value.trim().split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 || length == 0 {
            return Err(());
        }
        let count = suffix.min(length);
        return Ok(Some((length - count, count)));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= length {
        return Err(());
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some((start, end - start + 1)))
}
