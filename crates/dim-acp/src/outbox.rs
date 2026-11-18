//! Account reliable application output until the transport flushes each frame.
use agent_client_protocol::{
    Client, ConnectionTo, Error, RequestCancellation, Responder, UntypedMessage,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Pending {
    bytes: usize,
    frames: HashMap<String, VecDeque<usize>>,
}
#[derive(Clone, Default)]
pub(crate) struct Outbox {
    pending: Arc<Mutex<Pending>>,
    failed: CancellationToken,
    flushed: Arc<tokio::sync::Notify>,
}
fn overflow() -> Error {
    Error::new(-32001, "ACP reliable output exceeds its 16 MiB budget")
}
fn key(value: &Value) -> String {
    if let Some(id) = value.get("id") {
        format!(
            "{}:{id}",
            if value.get("method").is_some() {
                "request"
            } else {
                "response"
            }
        )
    } else {
        format!("notification:{}", value["method"])
    }
}
impl Outbox {
    fn reserve(&self, value: &Value) -> Result<(), Error> {
        let size = serde_json::to_vec(value)
            .map_err(|_| Error::internal_error())?
            .len()
            + 1;
        let mut pending = self.pending.lock().unwrap();
        if self.failed.is_cancelled() || pending.bytes + size > super::FRAME_LIMIT {
            self.failed.cancel();
            return Err(overflow());
        }
        pending.bytes += size;
        pending
            .frames
            .entry(key(value))
            .or_default()
            .push_back(size);
        Ok(())
    }
    pub(crate) async fn failed(&self) {
        self.failed.cancelled().await;
    }
    pub(crate) async fn drained(&self) -> Result<(), Error> {
        loop {
            let flushed = self.flushed.notified();
            let empty = self.pending.lock().unwrap().bytes == 0;
            if empty {
                return Ok(());
            }
            tokio::select! {
                () = flushed=>{},
                () = self.failed.cancelled()=>return Err(overflow()),
            }
        }
    }
    pub(crate) fn error(&self) -> Error {
        overflow()
    }
    pub(crate) fn flushed(&self, line: &str) {
        // Lines is the SDK's serialized transport boundary, including SDK-owned errors.
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let key = key(&value);
        let mut pending = self.pending.lock().unwrap();
        if let Some(queue) = pending.frames.get_mut(&key)
            && let Some(size) = queue.pop_front()
        {
            if queue.is_empty() {
                pending.frames.remove(&key);
            }
            pending.bytes -= size;
            if pending.bytes == 0 {
                self.flushed.notify_one();
            }
        }
    }
    pub(crate) fn notify(
        &self,
        cx: &ConnectionTo<Client>,
        method: &str,
        params: Value,
    ) -> Result<(), Error> {
        self.reserve(&json!({"jsonrpc":"2.0","method":method,"params":params}))?;
        cx.send_notification(UntypedMessage::new(method, params)?)
    }
    pub(crate) async fn request(
        &self,
        cx: &ConnectionTo<Client>,
        method: &str,
        params: Value,
    ) -> Result<Value, Error> {
        let request = cx.send_request(UntypedMessage::new(method, params.clone())?);
        self.reserve(&json!({"jsonrpc":"2.0","id":request.id(),"method":method,"params":params}))?;
        request.block_task().await
    }
}

pub(crate) struct Reply {
    responder: Arc<Mutex<Option<Responder>>>,
    outbox: Outbox,
    cancellation: RequestCancellation,
    done: CancellationToken,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Reply {
    pub(crate) fn new(
        responder: Responder,
        outbox: Outbox,
        tasks: &tokio_util::task::TaskTracker,
        stop: CancellationToken,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Self {
        let read = permit.is_some()
            && matches!(
                responder.method(),
                "session/list" | "session/load" | "session/resume" | "_lody/session/history/read"
            );
        let cancellation = responder.cancellation();
        let responder = Arc::new(Mutex::new(Some(responder)));
        let done = CancellationToken::new();
        if read {
            let responder = responder.clone();
            let outbox = outbox.clone();
            let cancellation = cancellation.clone();
            let done = done.clone();
            tasks.spawn(async move {
                tokio::select! {
                    () = stop.cancelled()=>{},
                    () = done.cancelled()=>{},
                    () = cancellation.cancelled()=>{
                        if let Some(responder) = responder.lock().unwrap().take() {
                            let _ = send(responder, Err(Error::request_cancelled()), &outbox);
                        }
                    }
                }
            });
        }
        Self {
            responder,
            outbox,
            cancellation,
            done,
            _permit: permit,
        }
    }
    pub(crate) fn cancellation(&self) -> RequestCancellation {
        self.cancellation.clone()
    }
    pub(crate) fn respond(self, value: impl serde::Serialize) -> Result<(), Error> {
        self.respond_with_result(Ok(value))
    }
    pub(crate) fn respond_with_error(self, error: Error) -> Result<(), Error> {
        self.respond_with_result(Err::<Value, Error>(error))
    }
    pub(crate) fn respond_with_result(
        self,
        result: Result<impl serde::Serialize, Error>,
    ) -> Result<(), Error> {
        let result = result
            .and_then(|value| serde_json::to_value(value).map_err(|_| Error::internal_error()));
        let Some(responder) = self.responder.lock().unwrap().take() else {
            return Ok(());
        };
        send(responder, result, &self.outbox)
    }
}
impl Drop for Reply {
    fn drop(&mut self) {
        self.done.cancel();
    }
}
fn send(responder: Responder, result: Result<Value, Error>, outbox: &Outbox) -> Result<(), Error> {
    let frame = match &result {
        Ok(value) => json!({"jsonrpc":"2.0","id":responder.id(),"result":value}),
        Err(error) => json!({"jsonrpc":"2.0","id":responder.id(),"error":error}),
    };
    outbox.reserve(&frame)?;
    responder.respond_with_result(result)
}
