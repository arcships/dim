use super::{Connection, ConnectionHub, Outbound, Reply};
use dim_agent_capabilities::methods::{RpcError, params};
use dim_agent_types::native_files::{Header, READY};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub(super) async fn receive(
    hub: &ConnectionHub,
    connection: &Connection,
    value: serde_json::Value,
    input: &mut (impl AsyncRead + Unpin),
) -> Result<Reply, RpcError> {
    let header: Header = params(value)?;
    if header.name.is_empty() || header.name.len() > 1024 || header.name.contains(['/', '\0']) {
        return Err(RpcError::invalid("Expected a file name"));
    }
    if header.byte_length > crate::storage::MAX_SESSION_FILE_BYTES as u64 {
        return Err(RpcError::app(
            -32000,
            "attachment_too_large",
            "Attachment exceeds 16 MiB",
        ));
    }
    let rpc = hub.with_rpc(Arc::clone)?;
    if !rpc.config.session_execution
        || rpc.stopping.is_cancelled()
        || connection.instance.get().map(String::as_str) != Some(rpc.index.server_instance_id())
    {
        return Err(RpcError::reloading());
    }
    connection.authorize(&rpc, &header.session_id)?;
    let _active = rpc
        .control
        .activity
        .take_hold()
        .ok_or_else(RpcError::reloading)?;
    let _permit = hub
        .uploads
        .try_acquire()
        .map_err(|_| RpcError::service("uploads_busy", "Upload concurrency limit reached"))?;
    let usage = rpc.index.acquire(&header.session_id).await?;
    let provider =
        crate::methods::session_provider(&rpc.loader, &usage.session, "attachment.import")?;
    let dir = tempfile::Builder::new()
        .prefix("upload-")
        .tempdir_in(&hub.upload_staging().await?.path)
        .map_err(io_error)?;
    let path = dir.path().join("upload");
    let mut file = tokio::fs::File::create_new(&path).await.map_err(io_error)?;
    connection
        .sender
        .send(
            Outbound::Message {
                jsonrpc: "2.0",
                method: READY.into(),
                params: Default::default(),
            },
            false,
        )
        .map_err(|_| RpcError::interrupted())?;
    let closing = hub.closing();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let receive = async {
        let mut remaining = header.byte_length;
        let mut buf = vec![0; 64 * 1024];
        while remaining > 0 {
            let limit = remaining.min(buf.len() as u64) as usize;
            let n = tokio::time::timeout(Duration::from_secs(30), input.read(&mut buf[..limit]))
                .await
                .map_err(|_| RpcError::invalid("Upload timed out"))?
                .map_err(io_error)?;
            if n == 0 {
                return Err(RpcError::invalid("Upload ended before the declared length"));
            }
            file.write_all(&buf[..n]).await.map_err(io_error)?;
            remaining -= n as u64;
        }
        file.flush().await.map_err(io_error)
    };
    tokio::select! {
        _ = usage.session.closing() => return Err(RpcError::interrupted()),
        _ = closing.cancelled() => return Err(RpcError::interrupted()),
        _ = rpc.stopping.cancelled() => return Err(RpcError::reloading()),
        _ = connection.closed.cancelled() => return Err(RpcError::interrupted()),
        result = tokio::time::timeout_at(deadline, receive) => {
            result.map_err(|_| RpcError::invalid("Upload timed out"))??;
        }
    }
    drop(file);
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Import<'a> {
        session_id: &'a str,
        path: &'a std::path::Path,
        image: bool,
        name: String,
    }
    let mut name = String::from("attachment");
    if let Some(ext) = std::path::Path::new(&header.name)
        .extension()
        .and_then(|s| s.to_str())
        && ext.len() <= 16
        && ext.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        name.push('.');
        name.push_str(ext);
    }
    let value = serde_json::to_value(Import {
        session_id: &header.session_id,
        path: &path,
        image: header.image,
        name,
    })
    .map_err(|e| RpcError::invalid(e.to_string()))?;
    let cancel = rpc.unloading.child_token();
    let guard = cancel.clone().drop_guard();
    let mut accepted = provider.accept(
        "attachment.import".into(),
        value,
        None,
        cancel.clone(),
        |_| {},
    )?;
    let mut extra = [0];
    let reply = tokio::select! {
        _ = tokio::time::sleep_until(deadline) => {cancel.cancel();Err(RpcError::invalid("Upload timed out"))},
        reply = &mut accepted.reply => reply.map_err(|_| RpcError::interrupted())?,
        _ = usage.session.closing() => { cancel.cancel(); Err(RpcError::interrupted()) },
        _ = input.read(&mut extra) => { cancel.cancel(); Err(RpcError::interrupted()) },
        _ = closing.cancelled() => { cancel.cancel(); Err(RpcError::interrupted()) }
    };
    let _ = accepted.settled.await;
    drop(guard);
    reply.map(Reply::Contributed)
}
fn io_error(error: std::io::Error) -> RpcError {
    RpcError::service("storage_failed", error.to_string())
}
