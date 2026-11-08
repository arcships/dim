//! The `modalityAdapter` binding (Modality A.2, Appendix G): the operations a
//! provider adapter serves, the job a task hands it, and what it returns.
//!
//! The neutral form in A.2 is normative; this module is the Rust binding. The
//! connection is passed per call and never stored, logged or returned: a task
//! resolves one `ModalityConnection` before each `submit`, `wait` and
//! `cancel_remote` and drops it when the call returns.
use dim_agent_capabilities::host::ModalityConnection;
use dim_agent_types::ModelId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A future an adapter operation answers. Cancelling `cancel` aborts the
/// call; an adapter stops its local work or its provider request when it can.
pub type AdapterFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AdapterError>> + Send + 'a>>;

/// One operation a Modality adapter serves for a model (A.2 `operations`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdapterOperation {
    pub category: String,
    pub operation: String,
    /// The JSON Schema of the operation's parameters (A.4); `{}` accepts any
    /// object. Modality validates a submission against it before anything is
    /// written.
    pub params_schema: Value,
}

/// One input or result file of a job (A.2). `path` is absolute: for an inline
/// input it lies under the task's `input/` directory, for a caller's
/// `node_path` it is the caller's own path, and a result lies inside the job's
/// work directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFile {
    pub name: String,
    pub media_type: String,
    pub path: PathBuf,
}

/// One asynchronous job handed to `submit` (A.2): the resolved model, the
/// validated parameters, the inputs and the directory the adapter writes its
/// results into.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub task_id: String,
    pub category: String,
    pub operation: String,
    pub model: ModelId,
    pub params: Value,
    pub files: Vec<JobFile>,
    pub work_dir: PathBuf,
}

/// What `submit` returns: the executor's handle when it has one. A task whose
/// adapter returns no handle cannot be resumed after a restart or reload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_handle: Option<String>,
}

/// What `wait` returns: the result files inside the job's work directory and
/// the operation's structured data, if any (A.2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AdapterOutput {
    #[serde(default)]
    pub files: Vec<JobFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The `code` of an adapter error (A.2): a violation of the request, an
/// operation that cannot run now, or the provider failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterErrorCode {
    InvalidInput,
    Unavailable,
    ProviderError,
}
impl AdapterErrorCode {
    /// The record's error code (A.4).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Unavailable => "unavailable",
            Self::ProviderError => "provider_error",
        }
    }
}

/// An adapter's error as data (A.2). Its message never carries credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterError {
    pub code: AdapterErrorCode,
    pub message: String,
}
impl AdapterError {
    pub fn new(code: AdapterErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(AdapterErrorCode::InvalidInput, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(AdapterErrorCode::Unavailable, message)
    }
    pub fn provider_error(message: impl Into<String>) -> Self {
        Self::new(AdapterErrorCode::ProviderError, message)
    }
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for AdapterError {}

/// What `cancel_remote` reports (A.2): the executor stopped the job, does not
/// support remote cancellation, or had already finished it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCancel {
    Cancelled,
    Unsupported,
    AlreadyFinished,
}
impl RemoteCancel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Unsupported => "unsupported",
            Self::AlreadyFinished => "already_finished",
        }
    }
}

/// One progress report an adapter sends while `wait` runs (A.2): `fraction`
/// of the work done and a display message. Modality writes it to the record
/// at most once per second (A10).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AdapterProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The progress channel of one `wait` call: the adapter reports progress and
/// Modality decides what to persist and what reaches a waiting client.
#[derive(Clone)]
pub struct ProgressSink(Arc<dyn Fn(AdapterProgress) + Send + Sync>);
impl ProgressSink {
    pub fn new(sink: impl Fn(AdapterProgress) + Send + Sync + 'static) -> Self {
        Self(Arc::new(sink))
    }
    /// A sink that drops the report, for calls nobody waits on.
    pub fn none() -> Self {
        Self(Arc::new(|_| {}))
    }
    pub fn send(&self, progress: AdapterProgress) {
        (self.0)(progress);
    }
}

/// One outbound realtime frame (A.2 `realtimeSend`): `text` is a text frame
/// carrying a JSON value, `audio` a binary frame carrying base64-decoded
/// bytes, and `control` a direct transport's signaling message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeFrame {
    pub kind: RealtimeFrameKind,
    pub data: Value,
}
impl RealtimeFrame {
    /// The bytes the frame puts on the wire: the JSON encoding of `data` for a
    /// text or control frame, and the decoded bytes for an audio frame.
    pub fn bytes(&self) -> Result<Vec<u8>, AdapterError> {
        match self.kind {
            RealtimeFrameKind::Text | RealtimeFrameKind::Control => serde_json::to_vec(&self.data)
                .map_err(|error| {
                    AdapterError::invalid_input(format!("The frame cannot be encoded: {error}"))
                }),
            RealtimeFrameKind::Audio => {
                let encoded = self.data.as_str().ok_or_else(|| {
                    AdapterError::invalid_input("An audio frame's data is a base64 string")
                })?;
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| AdapterError::invalid_input("An audio frame's data is not base64"))
            }
        }
    }
}

/// The kind of one outbound realtime frame (A.2 `realtimeSend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RealtimeFrameKind {
    Text,
    Audio,
    Control,
}

/// How the adapter serves a session (A.2 `realtimeOpen`): it holds the socket,
/// or the caller talks to the provider's own transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeTransport {
    Relay,
    Direct,
}

/// The short-lived connection a `direct` session returns (A.2): provider
/// material only, never the long-lived credential.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeConnection {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ice_servers: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// What a successful `realtimeOpen` returns: the channel later `send` and
/// `close` name, the transport, and a direct session's connection.
#[derive(Debug)]
pub struct RealtimeOpened {
    pub channel_id: String,
    pub transport: RealtimeTransport,
    pub connection: Option<RealtimeConnection>,
}

/// What an adapter reports on a session it opened.
pub enum RealtimeEvent {
    /// One provider event, forwarded to the opening connection verbatim.
    Event(Value),
    /// The provider ended the session on its own.
    Ended(RealtimeEnd),
}

/// Why an adapter's session ended without the caller asking: the provider
/// closed it, or the adapter/protocol failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeEnd {
    Provider,
    Error(String),
}

/// A process-scoped `modalityAdapter` (A.2, Appendix G). The first slice reads
/// the single process service per call; until the per-task lookup (#861) lands
/// the service is not keyed by adapter name.
pub trait ModalityAdapter: Send + Sync + 'static {
    /// Whether the adapter can run now, without a reload (A.2 `available`).
    fn available(&self) -> bool;
    /// The operations this adapter serves for `model`, each with its parameter
    /// schema (A.2 `operations`).
    fn operations(&self, model: &ModelId) -> Vec<AdapterOperation>;
    /// Starts one job (A.2 `submit`).
    fn submit(
        &self,
        job: Job,
        connection: ModalityConnection,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, Submitted>;
    /// Waits for the job `handle` names, or for the job `submit` just started
    /// when the adapter has no handle (A.2 `wait`). Progress is reported on
    /// `progress`; the call must work with a handle stored by an earlier
    /// generation.
    fn wait(
        &self,
        task_id: &str,
        handle: Option<&str>,
        connection: ModalityConnection,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, AdapterOutput>;
    /// Asks the executor to cancel the job `handle` names (A.2 `cancelRemote`).
    fn cancel_remote(
        &self,
        handle: &str,
        connection: ModalityConnection,
    ) -> AdapterFuture<'_, RemoteCancel>;
    /// Opens one realtime session (A.2 `realtimeOpen`). Events the provider
    /// sends are reported on `events` until the session ends; `cancel` aborts
    /// the open and later ends the session.
    fn realtime_open(
        &self,
        connection: ModalityConnection,
        params: Value,
        events: tokio::sync::mpsc::Sender<RealtimeEvent>,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, RealtimeOpened>;
    /// Sends one frame on the session `channel_id` names (A.2 `realtimeSend`).
    fn realtime_send(
        &self,
        channel_id: &str,
        frame: RealtimeFrame,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, ()>;
    /// Closes the session `channel_id` names (A.2 `realtimeClose`).
    fn realtime_close(&self, channel_id: &str) -> AdapterFuture<'_, ()>;
}
