//! The openai-realtime voice relay (A.2 `realtimeOpen`, `realtimeSend` and
//! `realtimeClose`): one socket per session, owned by one task.
//!
//! The catalog entry declares the transport and its path; the connection
//! supplies the base URL, the headers and the proxy. Nothing here keeps
//! either beyond a session: the socket task owns the [`WsConnection`] and is
//! the only place frames are written, so a cancelled `send` can never leave a
//! half-written frame behind.
use crate::Secrets;
use aimux_core::AbortSignal;
use aimux_core::options::TimeoutConfiguration;
use aimux_provider_utils::fetch::ProxyConfig;
use aimux_provider_utils::ws::{
    TungsteniteConnector, WebSocketRequest, WsConnection, WsConnector, WsMessage, peer_close,
    ws_connect,
};
use dim_agent_capabilities::host::{
    HttpModelConnection, ModalityConnection, ModelConnection, RealtimeDeclaration,
};
use dim_modality::{
    AdapterError, RealtimeEnd, RealtimeEvent, RealtimeFrame, RealtimeFrameKind, RealtimeOpened,
    RealtimeTransport,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// The protocol string a catalog entry names for this relay.
pub const PROTOCOL: &str = "openai-realtime";
/// How long the provider has to accept the socket and send its first event.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest provider message the socket accepts; a larger one is refused
/// by tungstenite while it is still being framed (D3).
pub const MAX_PROVIDER_MESSAGE_BYTES: usize = 1024 * 1024;
/// How long one `realtimeSend` may take before the session is abandoned
/// rather than the socket reused.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `realtimeClose` waits for the socket task; the close handshake
/// itself is bounded at five seconds inside [`WsConnection::close`].
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(6);

/// Channel ids are unique within the process; a session never outlives it.
static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

/// The relay's sessions, one per open (A.2).
#[derive(Default)]
pub(crate) struct Relay {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

struct Session {
    frames: mpsc::Sender<Outbound>,
    /// The session's own end: `realtimeClose`, or the socket task finishing.
    cancel: CancellationToken,
    /// Resolves when the socket task has closed the socket and reported.
    finished: CancellationToken,
}

/// One frame the socket task writes, and where its outcome goes.
enum Outbound {
    Frame(RealtimeFrame, oneshot::Sender<Result<(), AdapterError>>),
}

/// How a socket task's loop ended.
enum End {
    /// The relay closed it: `realtimeClose`, or a caller that went away.
    Closed,
    /// The provider closed the socket.
    Provider,
    /// The socket or the protocol failed.
    Error(String),
}

impl Relay {
    /// A.2 `realtimeOpen`: connect against the declaration the connection
    /// carries and hand the socket to its task.
    pub(crate) async fn open(
        &self,
        connection: &ModalityConnection,
        events: mpsc::Sender<RealtimeEvent>,
        cancel: &CancellationToken,
    ) -> Result<RealtimeOpened, AdapterError> {
        let Some(declaration) = &connection.realtime else {
            return Err(AdapterError::unavailable(
                "The model declares no realtime transport",
            ));
        };
        if declaration.protocol != PROTOCOL {
            return Err(AdapterError::unavailable(format!(
                "The adapter speaks {PROTOCOL}, not {}",
                declaration.protocol
            )));
        }
        let ModelConnection::Http(http) = &connection.connection else {
            return Err(AdapterError::unavailable(
                "The model's connection is local and serves no realtime session",
            ));
        };
        let secrets = Secrets::of(connection);
        let signal = AbortSignal::new();
        let request = WebSocketRequest {
            url: socket_url(http, declaration, &connection.model)?,
            headers: request_headers(http),
            subprotocols: Vec::new(),
            abort_signal: Some(signal.clone()),
            timeout: Some(TimeoutConfiguration {
                first_chunk_ms: Some(CONNECT_TIMEOUT.as_millis() as u64),
                ..Default::default()
            }),
            // The provider's messages are bounded at the socket (D3).
            max_message_bytes: Some(MAX_PROVIDER_MESSAGE_BYTES),
            connector: connector(http),
        };
        let socket = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(AdapterError::unavailable("The realtime connection was cancelled"));
            }
            result = ws_connect(&request) => {
                result.map_err(|error| connect_error(&secrets, &error))?
            }
        };
        let channel_id = format!("rt_{}", NEXT_CHANNEL.fetch_add(1, Ordering::SeqCst));
        let (frames, queue) = mpsc::channel(1);
        let session_cancel = CancellationToken::new();
        let finished = CancellationToken::new();
        self.sessions.lock().unwrap().insert(
            channel_id.clone(),
            Session {
                frames,
                cancel: session_cancel.clone(),
                finished: finished.clone(),
            },
        );
        tokio::spawn(serve(
            Task {
                sessions: self.sessions.clone(),
                channel_id: channel_id.clone(),
                events,
                cancel: session_cancel,
                finished,
                secrets,
                signal,
            },
            socket,
            queue,
        ));
        Ok(RealtimeOpened {
            channel_id,
            transport: RealtimeTransport::Relay,
            connection: None,
        })
    }

    /// A.2 `realtimeSend`: one frame through the socket task, bounded.
    pub(crate) async fn send(
        &self,
        channel_id: &str,
        frame: RealtimeFrame,
        cancel: &CancellationToken,
    ) -> Result<(), AdapterError> {
        // The openai-realtime protocol is text and audio only; a control
        // frame is a `direct` transport's signaling.
        if frame.kind == RealtimeFrameKind::Control {
            return Err(AdapterError::invalid_input(
                "The realtime relay carries text and audio frames, not control frames",
            ));
        }
        let frames = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .get(channel_id)
                .map(|session| session.frames.clone())
        };
        let Some(frames) = frames else {
            return Err(AdapterError::unavailable(
                "No realtime session is open on this channel",
            ));
        };
        let (ack, outcome) = oneshot::channel();
        let send = async {
            frames
                .send(Outbound::Frame(frame, ack))
                .await
                .map_err(|_| AdapterError::unavailable("The realtime session ended"))?;
            outcome
                .await
                .map_err(|_| AdapterError::unavailable("The realtime session ended"))?
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                Err(AdapterError::invalid_input("The realtime frame was cancelled"))
            }
            result = tokio::time::timeout(SEND_TIMEOUT, send) => match result {
                Ok(result) => result,
                Err(_) => {
                    // A frame the socket could not take in time abandons the
                    // session: the socket is not reused half-written.
                    self.end(channel_id);
                    Err(AdapterError::unavailable(
                        "The realtime session stopped accepting frames",
                    ))
                }
            },
        }
    }

    /// A.2 `realtimeClose`: end the session and wait, bounded, for the socket
    /// task to close the socket.
    pub(crate) async fn close(&self, channel_id: &str) -> Result<(), AdapterError> {
        let session = self.sessions.lock().unwrap().remove(channel_id);
        if let Some(session) = session {
            session.cancel.cancel();
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, session.finished.cancelled()).await;
        }
        Ok(())
    }

    /// Ends a session the relay abandons: the socket task reports the error.
    fn end(&self, channel_id: &str) {
        let session = self.sessions.lock().unwrap().remove(channel_id);
        if let Some(session) = session {
            session.cancel.cancel();
        }
    }
}

/// What one session's task shares with the methods.
struct Task {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    channel_id: String,
    events: mpsc::Sender<RealtimeEvent>,
    cancel: CancellationToken,
    finished: CancellationToken,
    secrets: Secrets,
    signal: AbortSignal,
}

/// One session's socket task (A.2): it owns the socket, writes every frame,
/// forwards every provider event, and ends the session exactly once.
async fn serve(task: Task, mut socket: WsConnection, mut queue: mpsc::Receiver<Outbound>) {
    let end = loop {
        // The select's handlers only record the step, so the socket is free
        // for the write or read the step then performs.
        let step = tokio::select! {
            biased;
            _ = task.cancel.cancelled() => Step::Closed,
            command = queue.recv() => match command {
                Some(command) => Step::Command(command),
                None => Step::Closed,
            },
            message = socket.next() => Step::Read(message),
        };
        match step {
            Step::Closed => break End::Closed,
            Step::Command(Outbound::Frame(frame, ack)) => {
                let result = write(&mut socket, frame, &task.secrets).await;
                let failed = result.as_ref().err().map(ToString::to_string);
                let _ = ack.send(result);
                if let Some(message) = failed {
                    break End::Error(message);
                }
            }
            Step::Read(None) => break End::Provider,
            Step::Read(Some(Ok(WsMessage::Text(text)))) => {
                let event: Value = match serde_json::from_str(&text) {
                    Ok(event) => event,
                    Err(_) => {
                        break End::Error("The provider sent a message that is not JSON".into());
                    }
                };
                if task.events.send(RealtimeEvent::Event(event)).await.is_err() {
                    break End::Closed;
                }
            }
            Step::Read(Some(Ok(WsMessage::Binary(_)))) => {
                break End::Error(
                    "The provider sent a binary message; the protocol is JSON text".into(),
                );
            }
            Step::Read(Some(Err(error))) => {
                if peer_close(&error).is_some() {
                    break End::Provider;
                }
                break End::Error(
                    task.secrets
                        .redact(&format!("The realtime socket failed: {error}")),
                );
            }
        }
    };
    // Whatever the reason, the socket is closed with a bounded handshake and
    // the session stops accepting frames.
    task.sessions.lock().unwrap().remove(&task.channel_id);
    socket.close().await;
    let ended = match end {
        End::Closed => None,
        End::Provider => Some(RealtimeEnd::Provider),
        End::Error(message) => Some(RealtimeEnd::Error(message)),
    };
    if let Some(ended) = ended {
        // The report is worth a bounded wait: a supervisor that stopped
        // consuming must not keep this task (and its `finished`) alive.
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            task.events.send(RealtimeEvent::Ended(ended)),
        )
        .await;
    }
    task.signal.abort();
    task.finished.cancel();
}

/// One select iteration: what the loop should do next.
enum Step {
    Closed,
    Command(Outbound),
    Read(Option<Result<WsMessage, aimux_core::AiMuxError>>),
}

/// Writes one frame. Text frames carry JSON, and an audio frame's data is
/// base64 that goes on the wire decoded.
async fn write(
    socket: &mut WsConnection,
    frame: RealtimeFrame,
    secrets: &Secrets,
) -> Result<(), AdapterError> {
    let failed = |error: aimux_core::AiMuxError| {
        AdapterError::provider_error(
            secrets.redact(&format!("The realtime frame was not sent: {error}")),
        )
    };
    match frame.kind {
        RealtimeFrameKind::Text => {
            let text = serde_json::to_string(&frame.data).map_err(|error| {
                AdapterError::invalid_input(format!("The frame cannot be encoded: {error}"))
            })?;
            socket.send_text(&text).await.map_err(failed)
        }
        RealtimeFrameKind::Audio => {
            let bytes = frame.bytes()?;
            socket.send_binary(&bytes).await.map_err(failed)
        }
        RealtimeFrameKind::Control => Err(AdapterError::invalid_input(
            "The realtime relay carries text and audio frames, not control frames",
        )),
    }
}

/// The socket URL: the connection's base URL, the declared path and the model
/// id as its query, with `http` and `https` mapped to `ws` and `wss`.
fn socket_url(
    http: &HttpModelConnection,
    declaration: &RealtimeDeclaration,
    model: &str,
) -> Result<String, AdapterError> {
    let mut url = url::Url::parse(&http.base_url)
        .map_err(|_| AdapterError::unavailable("The model's base URL is not a valid URL"))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => {
            return Err(AdapterError::unavailable(
                "The model's base URL is not http or https",
            ));
        }
    };
    url.set_scheme(scheme)
        .map_err(|_| AdapterError::unavailable("The model's base URL cannot be a socket"))?;
    let base = url.path().trim_end_matches('/').to_owned();
    let path = declaration.path.trim_start_matches('/');
    url.set_path(&format!("{base}/{path}"));
    url.set_fragment(None);
    url.query_pairs_mut().append_pair("model", model);
    Ok(url.to_string())
}

/// The request headers: every header the connection resolved, and the bearer
/// when Provider stripped one off its own header.
fn request_headers(http: &HttpModelConnection) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = http
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let authorized = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
    if let Some(bearer) = http.bearer.as_ref().filter(|_| !authorized) {
        headers.push(("Authorization".into(), format!("Bearer {bearer}")));
    }
    headers
}

/// The connector the session connects through: the connection's own proxy
/// when it names one, else the process default.
fn connector(http: &HttpModelConnection) -> Option<Arc<dyn WsConnector>> {
    http.network.proxy.as_ref().map(|proxy| {
        Arc::new(TungsteniteConnector::new(ProxyConfig {
            all_url: Some(proxy.clone()),
            ..ProxyConfig::default()
        })) as Arc<dyn WsConnector>
    })
}

/// A connect failure as the caller sees it: the HTTP status alone, never the
/// response body, and a redacted message otherwise.
fn connect_error(secrets: &Secrets, error: &aimux_core::AiMuxError) -> AdapterError {
    if let aimux_core::AiMuxError::ApiCall(call) = error
        && let Some(status) = call.status_code
    {
        return AdapterError::provider_error(format!(
            "The provider refused the realtime connection: HTTP {status}"
        ));
    }
    AdapterError::provider_error(
        secrets.redact(&format!("The realtime connection failed: {error}")),
    )
}
