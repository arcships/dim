//! The realtime layer of Modality (§3.3, A.2 `realtimeOpen`, `realtimeSend`
//! and `realtimeClose`, A.3): one session per `open`, the connection that owns
//! it, and the supervisor that owns its lifetime.
//!
//! A session is never stored and never resumed: it exists in this generation's
//! memory, and every way it can end passes through its supervisor, which is
//! the only place `modality.realtime.ended` is written.
use crate::ModalityAdapterService;
use crate::adapter::{
    AdapterError, AdapterErrorCode, RealtimeConnection, RealtimeEnd, RealtimeEvent, RealtimeFrame,
    RealtimeTransport,
};
use crate::engine::{Shared, invalid_input, not_found, resolve_task_model, unavailable};
use crate::{catalog_category, evaluate, unknown_operation};
use dim_agent_capabilities::host::{ModalityConnection, ModalityConnectionHost, ModalityTarget};
use dim_agent_capabilities::methods::{Caller, RpcError};
use dim_agent_types::ModelId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// At most this many sessions are open at once; `open` past it is
/// `realtime_limit` before any provider call (A9).
pub const DEFAULT_SESSIONS: usize = 4;
/// The largest frame a client sends, in decoded bytes (A9).
pub const DEFAULT_FRAME_BYTES: usize = 1024 * 1024;
/// How long a session may go without a `send` or a provider event (A9).
pub const DEFAULT_IDLE: Duration = Duration::from_secs(600);
/// How many provider events one session buffers before its adapter waits.
const EVENT_BUFFER: usize = 16;
/// How long ending a session waits for the adapter's `realtimeClose`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an unload waits for the supervisors past the adapter's own bound.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// The bounds on realtime sessions (A9). `Default` is the shipped set; a test
/// injects its own so a lifetime rule can be observed in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealtimeLimits {
    /// Sessions open at once, per process.
    pub sessions: usize,
    /// The largest frame a client sends, in decoded bytes.
    pub frame_bytes: usize,
    /// The longest silence between a `send` and a provider event.
    pub idle: Duration,
}
impl Default for RealtimeLimits {
    fn default() -> Self {
        Self {
            sessions: DEFAULT_SESSIONS,
            frame_bytes: DEFAULT_FRAME_BYTES,
            idle: DEFAULT_IDLE,
        }
    }
}

/// `modality.realtime.open` (A.3): the `realtime` operation, the model, and
/// the operation's parameters, which its adapter schema alone decides.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeOpen {
    pub operation: String,
    #[serde(default)]
    pub model: Option<ModelId>,
    #[serde(default = "empty_object")]
    pub params: Value,
}
fn empty_object() -> Value {
    json!({})
}

/// `modality.realtime.send` (A.3).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeSend {
    pub connection_id: String,
    pub frame: RealtimeFrame,
}

/// `modality.realtime.close` (A.3).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeClose {
    pub connection_id: String,
}

/// What `open` replies (A.3): the session, how the adapter serves it, and a
/// direct transport's short-lived connection.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeOpenedReply {
    pub connection_id: String,
    pub transport: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<RealtimeConnection>,
}

/// Why a session ended without the caller closing it. `Disconnected` is the
/// opening connection going away, which nothing can be written to; every
/// other reason is reported on `modality.realtime.ended`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Closed,
    Disconnected,
    Provider,
    Expired,
    Idle,
    Unloaded,
    Error,
}
impl End {
    /// The `reason` a client reads (A.3).
    fn reason(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Disconnected => "disconnected",
            Self::Provider => "provider",
            Self::Expired => "expired",
            Self::Idle => "idle",
            Self::Unloaded => "unloaded",
            Self::Error => "error",
        }
    }
    /// Whether the end can be reported: a closed connection is gone, so its
    /// sessions end silently.
    fn reported(self) -> bool {
        self != Self::Disconnected
    }
}

/// One open session: who owns it, the adapter channel behind it, and the
/// signals its supervisor and its methods share.
struct Session {
    id: String,
    /// The opening connection's id (A.3): only it may `send` or `close`.
    owner: String,
    /// The adapter's channel for this session (A.2).
    channel: String,
    adapter: Arc<ModalityAdapterService>,
    /// Wakes the supervisor when a `send` moves the idle deadline.
    activity: Notify,
    /// The session's own cancellation: `close`, or the end of its supervisor.
    cancel: CancellationToken,
    /// Resolves when the supervisor has done everything it owes this session.
    finished: CancellationToken,
}

/// The realtime sessions of one Modality generation (A.3, A.9).
pub(crate) struct Realtime {
    shared: Arc<Shared>,
    limits: RealtimeLimits,
    /// One permit per open session; the cap is the semaphore's.
    slots: Arc<Semaphore>,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    /// The generation's own stop: every supervisor ends on it with
    /// `unloaded`, before the task layer's shutdown runs.
    stop: CancellationToken,
}

impl Realtime {
    pub(crate) fn new(shared: Arc<Shared>, limits: RealtimeLimits) -> Self {
        Self {
            shared,
            limits,
            slots: Arc::new(Semaphore::new(limits.sessions)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            stop: CancellationToken::new(),
        }
    }

    /// `modality.realtime.open` (A.3): the cap, the model, the adapter's
    /// schema, the connection and the declaration the adapter needs, in that
    /// order; nothing reaches the provider before every step holds.
    pub(crate) async fn open(
        self: &Arc<Self>,
        request: RealtimeOpen,
        caller: Caller,
        cancel: &CancellationToken,
    ) -> Result<RealtimeOpenedReply, RpcError> {
        if self.stop.is_cancelled() {
            return Err(RpcError::reloading());
        }
        // A session is owned by the connection that opened it, and its events
        // go there alone (A.3).
        let owner = caller.id().to_owned();
        let category =
            catalog_category("realtime").expect("the realtime category is in the catalog");
        let row = category
            .rows
            .iter()
            .find(|row| row.operation == request.operation)
            .ok_or_else(|| {
                unknown_operation(format!("Unknown realtime operation {}", request.operation))
            })?;
        if !request.params.is_object() {
            return Err(invalid_input("params must be an object"));
        }
        // The cap (A9): past it, no model is resolved and no provider is
        // called.
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            RpcError::app(
                -32000,
                "realtime_limit",
                format!(
                    "At most {} realtime sessions run at once",
                    self.limits.sessions
                ),
            )
        })?;
        let (_, evaluation) = evaluate(
            &self.shared.settings,
            &self.shared.selectable,
            &self.shared.selection,
            &self.shared.ctx,
            self.shared.ocr_helper.as_deref(),
            cancel,
            // A realtime session never reaches the OCR target (A.2.1).
            crate::OcrLookup::Skip,
        )
        .await?;
        let model = resolve_task_model(&evaluation, category.name, row, request.model.as_ref())?;
        let Some(spec) = evaluation.operation_spec(&model, category.name, row.operation) else {
            return Err(unavailable(format!(
                "No available adapter serves {} for this model",
                row.id(category.name)
            )));
        };
        crate::engine::check_spec_params(&spec, &request.params, &row.id(category.name))?;
        let Some(adapter) = evaluation.adapter().cloned() else {
            return Err(unavailable(format!(
                "No available adapter serves {}",
                row.id(category.name)
            )));
        };
        let connection = self.connection(&model, row, cancel).await?;
        // A `realtime.*` model whose entry declares no transport cannot be
        // opened; nothing has been sent to the provider yet.
        if connection.realtime.is_none() {
            return Err(unavailable("The model declares no realtime transport"));
        }
        let cancel_session = CancellationToken::new();
        let (events_tx, events_rx) = mpsc::channel(EVENT_BUFFER);
        let opened = tokio::select! {
            result =
                adapter
                    .0
                    .realtime_open(connection, request.params, events_tx, cancel_session.child_token()) =>
            {
                match result {
                    Ok(opened) => opened,
                    Err(error) => {
                        cancel_session.cancel();
                        return Err(adapter_error(error));
                    }
                }
            }
            _ = cancel.cancelled() => {
                cancel_session.cancel();
                return Err(RpcError::interrupted());
            }
        };
        let id = format!("rc_{}", uuid::Uuid::new_v4());
        let expires = opens_until(&opened.connection, &self.shared);
        let session = Arc::new(Session {
            id: id.clone(),
            owner,
            channel: opened.channel_id,
            adapter,
            activity: Notify::new(),
            cancel: cancel_session,
            finished: CancellationToken::new(),
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        // The supervisor starts before the reply is written: an event that
        // arrives first is held on the connection and flushed after it.
        self.spawn(session, events_rx, permit, caller, expires);
        Ok(RealtimeOpenedReply {
            connection_id: id,
            transport: opened.transport.as_str(),
            connection: opened.connection,
        })
    }

    /// `modality.realtime.send` (A.3): the session's owner sends one frame,
    /// bounded before the adapter sees it.
    pub(crate) async fn send(
        &self,
        request: RealtimeSend,
        caller: &Caller,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let session = self.owned(&request.connection_id, caller)?;
        let bytes = request.frame.bytes().map_err(adapter_error)?;
        if bytes.len() > self.limits.frame_bytes {
            return Err(invalid_input(format!(
                "A realtime frame is at most {} bytes",
                self.limits.frame_bytes
            )));
        }
        session.activity.notify_one();
        let send_cancel = cancel.child_token();
        tokio::select! {
            result = session
                .adapter
                .0
                .realtime_send(&session.channel, request.frame, send_cancel) =>
            {
                result.map_err(adapter_error)?;
                Ok(json!({}))
            }
            _ = cancel.cancelled() => Err(RpcError::interrupted()),
        }
    }

    /// `modality.realtime.close` (A.3): the session's owner ends it. The
    /// adapter's close handshake and the `ended` notice are the supervisor's.
    pub(crate) fn close(&self, request: RealtimeClose, caller: &Caller) -> Result<Value, RpcError> {
        let session = self.owned(&request.connection_id, caller)?;
        self.sessions.lock().unwrap().remove(&request.connection_id);
        session.cancel.cancel();
        Ok(json!({}))
    }

    /// The session `connection_id` names, when `caller`'s connection opened
    /// it. Another connection's id is not found, as an unknown one is: a
    /// session is visible to its owner alone (A.3).
    fn owned(&self, connection_id: &str, caller: &Caller) -> Result<Arc<Session>, RpcError> {
        let session = self.sessions.lock().unwrap().get(connection_id).cloned();
        match session {
            Some(session) if session.owner == caller.id() => Ok(session),
            _ => Err(not_found(format!(
                "No realtime session {connection_id} on this connection"
            ))),
        }
    }

    /// The connection one `realtime.<operation>` call resolves (§3.4, #799):
    /// never stored, logged or returned.
    async fn connection(
        &self,
        model: &ModelId,
        row: &crate::OperationRow,
        cancel: &CancellationToken,
    ) -> Result<ModalityConnection, RpcError> {
        let Some(host) = self.shared.ctx.get::<ModalityConnectionHost>() else {
            return Err(unavailable(
                "Provider does not serve a non-conversational connection",
            ));
        };
        host.0
            .resolve_for_modality(
                ModalityTarget {
                    target: model.clone(),
                    operation: row.id("realtime"),
                },
                cancel.child_token(),
            )
            .await
            .map_err(crate::host_error)
    }

    /// Starts the supervisor of one session.
    fn spawn(
        self: &Arc<Self>,
        session: Arc<Session>,
        events: mpsc::Receiver<RealtimeEvent>,
        permit: OwnedSemaphorePermit,
        caller: Caller,
        expires: Option<tokio::time::Instant>,
    ) {
        let realtime = self.clone();
        tokio::spawn(supervise(
            realtime, session, events, permit, caller, expires,
        ));
    }

    /// The unload rule (§3.3): every session ends here, before the task layer
    /// stops, so each one can still report `unloaded` to its connection.
    pub(crate) async fn shutdown(&self) {
        self.stop.cancel();
        let sessions: Vec<Arc<Session>> = self.sessions.lock().unwrap().values().cloned().collect();
        let deadline = tokio::time::Instant::now() + CLOSE_TIMEOUT + SHUTDOWN_GRACE;
        for session in &sessions {
            let _ = tokio::time::timeout_at(deadline, session.finished.cancelled()).await;
        }
    }
}

impl RealtimeTransport {
    /// The `transport` a client reads (A.3).
    fn as_str(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Direct => "direct",
        }
    }
}

/// When a `direct` session's connection expires, as a deadline this process's
/// clock can wait on. A missing or unparsable `expires_at` leaves the session
/// to its other lifetime rules.
fn opens_until(
    connection: &Option<RealtimeConnection>,
    shared: &Shared,
) -> Option<tokio::time::Instant> {
    let text = connection.as_ref()?.expires_at.as_deref()?;
    let at = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let delta = at.with_timezone(&chrono::Utc) - shared.clock.now();
    let delta = delta.to_std().unwrap_or_default();
    Some(tokio::time::Instant::now() + delta)
}

fn adapter_error(error: AdapterError) -> RpcError {
    match error.code {
        AdapterErrorCode::InvalidInput => invalid_input(error.message),
        AdapterErrorCode::Unavailable => unavailable(error.message),
        AdapterErrorCode::ProviderError => RpcError::app(-32000, "provider_error", error.message),
    }
}

/// One session's lifetime (§3.3, A9): it forwards provider events to the
/// opening connection and ends on the first of the caller closing, `close`,
/// the provider, `expires_at`, the idle bound or the generation unloading.
async fn supervise(
    realtime: Arc<Realtime>,
    session: Arc<Session>,
    mut events: mpsc::Receiver<RealtimeEvent>,
    permit: OwnedSemaphorePermit,
    caller: Caller,
    expires: Option<tokio::time::Instant>,
) {
    let limits = realtime.limits;
    let mut idle_at = tokio::time::Instant::now() + limits.idle;
    let end = loop {
        tokio::select! {
            _ = caller.closed() => break End::Disconnected,
            _ = realtime.stop.cancelled() => break End::Unloaded,
            _ = session.cancel.cancelled() => break End::Closed,
            _ = session.activity.notified() => {
                idle_at = tokio::time::Instant::now() + limits.idle;
            }
            _ = expiry(expires) => break End::Expired,
            event = events.recv() => {
                match event {
                    Some(RealtimeEvent::Event(event)) => {
                        idle_at = tokio::time::Instant::now() + limits.idle;
                        if caller
                            .notify("modality.realtime.event", event_params(&session.id, event))
                            .is_err()
                        {
                            break End::Disconnected;
                        }
                    }
                    Some(RealtimeEvent::Ended(RealtimeEnd::Provider)) => break End::Provider,
                    Some(RealtimeEvent::Ended(RealtimeEnd::Error(message))) => {
                        // The adapter's message never carries a credential; it
                        // is logged and never written to the connection.
                        tracing::warn!(session = %session.id, %message, "realtime session failed");
                        break End::Error;
                    }
                    // The adapter dropped its sender without reporting an end.
                    None => break End::Error,
                }
            }
            _ = tokio::time::sleep_until(idle_at) => break End::Idle,
        }
    };
    // No method can reach the session once it has left the map.
    {
        let mut sessions = realtime.sessions.lock().unwrap();
        if sessions
            .get(&session.id)
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            sessions.remove(&session.id);
        }
    }
    // The adapter's side ends under its own bound: a socket that cannot close
    // in time is abandoned rather than awaited forever.
    let _ = tokio::time::timeout(
        CLOSE_TIMEOUT,
        session.adapter.0.realtime_close(&session.channel),
    )
    .await;
    if end.reported() {
        let mut params = Map::new();
        params.insert("connectionId".into(), json!(session.id));
        params.insert("reason".into(), json!(end.reason()));
        let _ = caller.notify("modality.realtime.ended", params);
    }
    drop(permit);
    session.finished.cancel();
}

/// Resolves at `expires`, or never when the session has none.
async fn expiry(expires: Option<tokio::time::Instant>) {
    match expires {
        Some(expires) => tokio::time::sleep_until(expires).await,
        None => std::future::pending().await,
    }
}

fn event_params(connection_id: &str, event: Value) -> Map<String, Value> {
    let mut params = Map::new();
    params.insert("connectionId".into(), json!(connection_id));
    params.insert("event".into(), event);
    params
}
