//! One owned native connection multiplexes calls and reliable notifications.

use agent_client_protocol::Error;
use dim_client::{Client, Message};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) enum Native {
    Notice {
        method: String,
        params: Value,
    },
    Lost,
    Restored {
        same_instance: bool,
        generation: u64,
        deadline: tokio::time::Instant,
    },
    Closed,
}

struct Call {
    generation: u64,
    method: &'static str,
    params: Value,
    reply: oneshot::Sender<Result<Value, Error>>,
}

#[derive(Clone)]
pub(crate) struct Rpc {
    calls: mpsc::Sender<Call>,
    state: Arc<RwLock<State>>,
}
struct State {
    generation: u64,
    ready: bool,
    features: Vec<String>,
}

pub(crate) fn unavailable() -> Error {
    Error::new(
        -32001,
        "App Server connection is unavailable; a submitted operation may have been accepted",
    )
    .data(json!({"code":"daemon_unavailable"}))
}

pub(crate) fn boundary(error: dim_client::Error) -> Error {
    if let Some(error) = error.downcast_ref::<dim_client::RpcError>() {
        let app = error
            .data
            .as_ref()
            .and_then(|data| data.get("appError"))
            .and_then(Value::as_str);
        let code = match error.code {
            -32601 | -32602 => error.code,
            _ => -32001,
        };
        Error::new(code, error.message.clone())
            .data(json!({"code":"native_error","nativeAppError":app}))
    } else {
        unavailable()
    }
}

impl Rpc {
    pub(crate) fn start(
        client: Client,
        tasks: &TaskTracker,
        stop: CancellationToken,
    ) -> (Self, mpsc::Receiver<Native>) {
        let state = Arc::new(RwLock::new(State {
            generation: 0,
            ready: true,
            features: client.features().to_vec(),
        }));
        let (calls, incoming) = mpsc::channel(128);
        let (notices, events) = mpsc::channel(256);
        tasks.spawn(drive(client, incoming, notices, stop, state.clone()));
        (Self { calls, state }, events)
    }

    pub(crate) fn supports(&self, method: &str) -> bool {
        // These v3 baseline calls are not repeated in hello.features.
        matches!(
            method,
            "session.attach"
                | "session.detach"
                | "session.submit"
                | "session.stop"
                | "session.list"
                | "session.close"
        ) || self
            .state
            .read()
            .unwrap()
            .features
            .iter()
            .any(|feature| feature == method)
    }

    pub(crate) async fn call(&self, method: &'static str, params: Value) -> Result<Value, Error> {
        let generation = {
            let state = self.state.read().unwrap();
            if !state.ready {
                return Err(unavailable());
            }
            state.generation
        };
        let (reply, result) = oneshot::channel();
        self.calls
            .send(Call {
                generation,
                method,
                params,
                reply,
            })
            .await
            .map_err(|_| unavailable())?;
        result.await.map_err(|_| unavailable())?
    }
    pub(crate) fn generation(&self) -> u64 {
        self.state.read().unwrap().generation
    }
    pub(crate) fn ready(&self) -> bool {
        self.state.read().unwrap().ready
    }

    pub(crate) async fn typed<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<T, Error> {
        let result = self.call(method, params).await?;
        serde_json::from_value(result).map_err(|_| {
            Error::internal_error().data(json!({"code":"invalid_native_response","method":method}))
        })
    }
}

async fn drive(
    mut client: Client,
    mut calls: mpsc::Receiver<Call>,
    notices: mpsc::Sender<Native>,
    stop: CancellationToken,
    state: Arc<RwLock<State>>,
) {
    let config = client.connection_config().clone();
    let mut attached = HashSet::new();
    let mut usable = true;
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value, Error>>> = HashMap::new();
    'connection: loop {
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break 'connection,
                call = calls.recv(), if pending.len() < 128 => {
                    let Some(call) = call else { break 'connection };
                    if call.generation != state.read().unwrap().generation {
                        let _ = call.reply.send(Err(unavailable()));
                        continue;
                    }
                    track(&mut attached, &call);
                    let sent = tokio::select! {
                        biased;
                        () = stop.cancelled() => {
                            usable = false;
                            let _ = call.reply.send(Err(unavailable()));
                            break 'connection;
                        }
                        sent = client.send(call.method, call.params) => sent,
                    };
                    match sent {
                        Ok(id) => { pending.insert(id, call.reply); }
                        Err(error) => {
                            let oversized = error
                                .downcast_ref::<dim_client::RequestTooLarge>()
                                .is_some();
                            let _ = call.reply.send(Err(send_error(error)));
                            if !oversized { break; }
                        }
                    }
                }
                message = client.next() => match message {
                    Ok(Message::Response { id, result }) => {
                        let Some(reply) = pending.remove(&id) else { break 'connection };
                        let _ = reply.send(result.map_err(|error| boundary(Box::new(error))));
                    }
                    Ok(Message::Notice { method, params }) => {
                        let notice = Native::Notice { method, params };
                        if notices.try_send(notice).is_err() { break 'connection; }
                    }
                    Err(error) if error.downcast_ref::<std::io::Error>().is_some() => break,
                    Err(_) => break 'connection,
                }
            }
        }
        {
            let mut state = state.write().unwrap();
            state.ready = false;
            state.generation += 1;
        }
        usable = false;
        for (_, reply) in pending.drain() {
            let _ = reply.send(Err(unavailable()));
        }
        if notices.try_send(Native::Lost).is_err() {
            break;
        }
        let instance = client.instance_id().to_owned();
        attached.clear();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let reconnect = async {
            let mut delay = Duration::from_millis(250);
            loop {
                tokio::time::sleep(delay).await;
                if let Ok(client) = Client::connect(&config).await {
                    return client;
                }
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        };
        tokio::pin!(reconnect);
        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                () = stop.cancelled() => break 'connection,
                () = &mut timeout => break 'connection,
                restored = &mut reconnect => {
                    client = restored;
                    usable = true;
                    {
                        let mut state = state.write().unwrap();
                        state.ready = true;
                        state.features = client.features().to_vec();
                    }
                    let restored = Native::Restored {
                        same_instance: client.instance_id() == instance,
                        generation: state.read().unwrap().generation,
                        deadline,
                    };
                    if notices.try_send(restored).is_err() { break 'connection; }
                    break;
                }
                call = calls.recv() => {
                    let Some(call) = call else { break 'connection };
                    let _ = call.reply.send(Err(unavailable()));
                }
            }
        }
    }
    state.write().unwrap().ready = false;
    for reply in pending.into_values() {
        let _ = reply.send(Err(unavailable()));
    }
    if stop.is_cancelled() && usable {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for id in attached {
                let _ = client.rpc("session.detach", json!({"sessionId":id})).await;
            }
        })
        .await;
    }
    let _ = notices.try_send(Native::Closed);
}

/// Records the Session a call binds, so a graceful close detaches every one.
fn track(attached: &mut HashSet<String>, call: &Call) {
    let Some(id) = call.params.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    match call.method {
        "session.attach" => {
            attached.insert(id.to_owned());
        }
        "session.detach" => {
            attached.remove(id);
        }
        _ => {}
    }
}

/// Why a call could not be sent: an oversized request is the caller's fault
/// and the connection survives it, anything else is the connection's.
fn send_error(error: dim_client::Error) -> Error {
    if error
        .downcast_ref::<dim_client::RequestTooLarge>()
        .is_some()
    {
        Error::new(-32602, "App Server request exceeds the 16 MiB frame limit")
            .data(json!({"code":"request_too_large"}))
    } else {
        boundary(error)
    }
}
